// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Imperative PostgreSQL I/O for shared databases (2.29 / ADR 0066 §3.1).
//!
//! The third imperative client in this crate, after Redis (ADR 0042) and NATS
//! (ADR 0061), and it exists for the same kind of reason: the thing it manages
//! cannot be declared on a CR. CNPG's `managed.roles` creates exactly one role
//! here — the platform's own — because a `CREATEROLE` role without superuser
//! may administer only the roles it CREATED (measured;
//! `docs/measurements/2.29-shared-database-2026-09-15.md`). Groups, consumer
//! roles and their grants are therefore ours to run.
//!
//! Shaped like [`crate::redis_client`]: a [`PgAdmin`] trait so the reconcile
//! logic is unit-testable against a fake, and a production client over
//! `tokio-postgres`.
//!
//! # Credentials never reach an error or a log
//!
//! A statement from [`crate::shared_pg::bind_consumer`] CONTAINS a password —
//! it must, because `CREATE ROLE` is a utility statement PostgreSQL will not
//! parameterise. So no variant of [`PgAdminError`] carries statement text, and
//! the failing statement is identified by its INDEX in the batch. That is
//! enough to find it (the batch is deterministic) and cannot leak.
//!
//! The DSN carries a password too, so it is never interpolated into an error
//! either — errors name the HOST, which the DSN's caller already knows.

use std::future::Future;
use std::time::Duration;

use async_trait::async_trait;
use thiserror::Error;
use tokio_postgres::error::SqlState;

/// How long the TCP dial may take, per resolved address.
///
/// `tokio-postgres` has no default: a host that drops SYNs is otherwise
/// bounded only by the kernel's SYN retries (about 127s on Linux). Note what
/// this does NOT cover — the DNS lookup runs before it, and the startup
/// handshake after it. [`CALL_TIMEOUT`] is the bound that covers both.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Server-side bounds every session this client opens carries, sent as the
/// startup `options` parameter.
///
/// These are the bounds that actually free the SERVER. A client-side timeout
/// only stops waiting: measured on PostgreSQL 18.6 (WI-400 mapping), a
/// statement waiting on a lock kept its backend after its client was cut and
/// after its socket was closed, and only a server-side `lock_timeout` (or a
/// CancelRequest) released it. `GRANT SELECT ON ALL TABLES` in
/// [`crate::shared_pg::grant_reader`] waits behind any consumer transaction
/// that touched a table — a migration, an idle-in-transaction app — so
/// without `lock_timeout` a routine reconcile waits as long as the tenant
/// does, and a cut one leaves a backend holding a slot on the shared
/// cluster's `max_connections`.
///
/// `lock_timeout` well under `statement_timeout`: a lock wait is the case
/// expected in practice and should report as one (SQLSTATE 55P03), not as a
/// generic slow statement (57014).
///
/// Every session carries them, the two DELETE paths included, and there they
/// change what a lock wait costs: both used to wait a lock out, and now a
/// lock held past `lock_timeout` cancels the statement. The SharedDatabase's
/// own `drop_groups` loses nothing by that, because it holds its finalizer
/// and retries on any failure. A consumer claim's `revoke_consumer` is
/// best-effort and releases the claim's finalizer on any failure, so a lock
/// wait there leaves the consumer's LOGIN role on the server, logged at WARN
/// with the role named.
pub const SESSION_OPTIONS: &str = "-c lock_timeout=10s -c statement_timeout=30s";

/// The bound on one whole [`PgAdmin`] call — DNS, dial, startup, every
/// statement, and the clean close.
///
/// Above [`CONNECT_TIMEOUT`] + `statement_timeout` (40s), so on an answering
/// server the server-side bounds fire first and report what happened. This
/// one catches what they cannot: a DNS lookup that hangs, and a peer that
/// accepts the connection and then says nothing.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(45);

/// Which of [`SESSION_OPTIONS`]' two bounds the server enforced.
///
/// They call for different things, which is why the caller is told which:
/// a lock wait ends when the transaction holding the lock does, while a
/// statement that runs past `statement_timeout` on its own may do so on every
/// attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerBound {
    /// `lock_timeout` — SQLSTATE 55P03 `lock_not_available`: another
    /// transaction held a lock the statement needed.
    Lock,
    /// `statement_timeout` — SQLSTATE 57014 `query_canceled`: the statement
    /// itself ran past the bound (57014 is also what an operator's
    /// `pg_cancel_backend` produces; `cause` tells the two apart).
    Statement,
}

/// Errors from the imperative Postgres admin path. Generic by construction:
/// no variant carries a statement or a DSN, so logging one is leak-safe.
#[derive(Debug, Error)]
pub enum PgAdminError {
    /// Could not connect (DNS, dial, auth, TLS).
    #[error("postgres connect to {host} failed: {source}")]
    Connect {
        host: String,
        #[source]
        source: tokio_postgres::Error,
    },
    /// A statement failed. Identified by its INDEX in the batch, never by its
    /// text — statement `index` of a bind batch carries the consumer's
    /// password.
    #[error("postgres statement #{index} of {total} failed on {host}: {source}")]
    Statement {
        index: usize,
        total: usize,
        host: String,
        #[source]
        source: tokio_postgres::Error,
    },
    /// The SERVER cancelled statement `index` on one of [`SESSION_OPTIONS`]'
    /// bounds — `bound` says which.
    ///
    /// Its own variant because it is a different finding from
    /// [`Self::Statement`] and from [`Self::Connect`]: the server is up and
    /// the database exists. A caller that reported it as "not answering" or
    /// "not created yet" would be saying something false. `cause` is the
    /// server's own message ("canceling statement due to lock timeout"),
    /// which names no statement text.
    #[error("postgres statement #{index} of {total} was cancelled by {host}: {cause}")]
    ServerCancelled {
        index: usize,
        total: usize,
        host: String,
        bound: ServerBound,
        cause: String,
    },
    /// The whole call ran past [`CALL_TIMEOUT`] and was abandoned. Its
    /// connection was closed with it (see [`Driver`]).
    #[error("postgres call to {host} did not finish within {}s", .after.as_secs())]
    TimedOut { host: String, after: Duration },
    /// The connection task ended before the work did.
    #[error("postgres connection to {host} closed early")]
    ConnectionLost { host: String },
}

/// The imperative Postgres operations a shared database needs.
///
/// Every one is idempotent, because a controller re-runs them: the statement
/// builders guard each `CREATE` behind an existence check and express a
/// password reset as the `ELSE` branch of the same block.
#[async_trait]
pub trait PgAdmin: Send + Sync {
    /// Run `statements` in order on `dsn`, stopping at the first failure.
    ///
    /// NOT wrapped in a transaction, deliberately: `CREATE DATABASE` cannot
    /// run inside one, and a batch that is half transactional and half not
    /// would be harder to reason about than one that is simply re-runnable.
    /// Every statement is individually idempotent instead, so a partial batch
    /// is completed by the next reconcile rather than rolled back.
    async fn execute_all(&self, dsn: &str, statements: &[String]) -> Result<(), PgAdminError>;

    /// Whether the running server provides `extension`.
    ///
    /// Asked rather than assumed: whether `vector` exists is a property of the
    /// operand IMAGE, the image is not pinned, and a CNPG bump can take it away
    /// under a live database (ADR 0066 §4.2).
    async fn extension_available(&self, dsn: &str, extension: &str) -> Result<bool, PgAdminError>;
}

/// The outcome of asking a live server about a set of declared extensions.
///
/// Three states rather than a `Vec<String>`, because "the server says none
/// are missing" and "the server did not answer" must not collapse into the
/// same empty list. They call for opposite actions: the first is a clean
/// result, the second is a reason to say nothing and ask again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtensionProbe {
    /// Every declared extension is available — or none was declared.
    AllPresent,
    /// The server answered, and these are absent. Never empty.
    Missing(Vec<String>),
    /// The server could not be reached. NOT a finding about extensions.
    Unreachable,
}

/// Ask `dsn` which of `declared` it provides (ADR 0066 §4.2).
///
/// Shared by the owned-database arm and the `SharedDatabase` controller so
/// that one definition fixes what best-effort means here. A server we cannot
/// reach is not the same finding as an extension that does not exist, and
/// failing a claim on a transient dial would turn a network blip into a
/// provisioning error — so the first connection failure ends the probe as
/// [`ExtensionProbe::Unreachable`] and the next reconcile asks again.
pub async fn probe_extensions(
    pg: &dyn PgAdmin,
    dsn: &str,
    declared: &[operator_core::PgExtension],
) -> ExtensionProbe {
    let mut missing: Vec<String> = Vec::new();
    for ext in declared {
        match pg.extension_available(dsn, &ext.name).await {
            Ok(true) => {}
            // The name reported is the one the MANIFEST used, not a folded
            // copy: the reader has to find this string in their own file.
            Ok(false) => missing.push(ext.name.clone()),
            Err(_) => return ExtensionProbe::Unreachable,
        }
    }
    if missing.is_empty() {
        ExtensionProbe::AllPresent
    } else {
        ExtensionProbe::Missing(missing)
    }
}

/// The host part of a DSN, for error messages. Never the whole DSN — that
/// carries the password.
fn host_of(dsn: &str) -> String {
    dsn.rsplit_once('@')
        .map(|(_, after)| after)
        .unwrap_or(dsn)
        .split('/')
        .next()
        .unwrap_or("unknown")
        .to_string()
}

/// The connection config every [`PgClient`] session uses: `dsn` plus
/// [`CONNECT_TIMEOUT`] and [`SESSION_OPTIONS`].
///
/// Applied here rather than in `cnpg::dsn`, which also renders the DSN an
/// APPLICATION connects with: the server-side bounds are this client's
/// policy, and a tenant's own sessions must not inherit a 30s
/// `statement_timeout` from the platform.
fn admin_config(dsn: &str) -> Result<tokio_postgres::Config, tokio_postgres::Error> {
    let mut config: tokio_postgres::Config = dsn.parse()?;
    config
        .connect_timeout(CONNECT_TIMEOUT)
        .options(SESSION_OPTIONS);
    Ok(config)
}

/// Classify a failed statement: the server's own cancellation on one of
/// [`SESSION_OPTIONS`]' bounds is [`PgAdminError::ServerCancelled`], anything
/// else is [`PgAdminError::Statement`].
fn statement_error(
    index: usize,
    total: usize,
    host: &str,
    source: tokio_postgres::Error,
) -> PgAdminError {
    let cancelled = source.as_db_error().and_then(|db| {
        let bound = if *db.code() == SqlState::LOCK_NOT_AVAILABLE {
            ServerBound::Lock
        } else if *db.code() == SqlState::QUERY_CANCELED {
            ServerBound::Statement
        } else {
            return None;
        };
        Some((bound, db.message().to_string()))
    });
    match cancelled {
        Some((bound, cause)) => PgAdminError::ServerCancelled {
            index,
            total,
            host: host.to_string(),
            bound,
            cause,
        },
        None => PgAdminError::Statement {
            index,
            total,
            host: host.to_string(),
            source,
        },
    }
}

/// Run one whole call under [`CALL_TIMEOUT`].
async fn bounded<T>(
    host: &str,
    call: impl Future<Output = Result<T, PgAdminError>>,
) -> Result<T, PgAdminError> {
    match tokio::time::timeout(CALL_TIMEOUT, call).await {
        Ok(finished) => finished,
        Err(_elapsed) => Err(PgAdminError::TimedOut {
            host: host.to_string(),
            after: CALL_TIMEOUT,
        }),
    }
}

/// The spawned task that drives one connection's socket, ABORTED when dropped.
///
/// `tokio-postgres` splits a connection into a `Client` and a `Connection`
/// future that must be polled for anything to move, so the future is spawned.
/// A plain `JoinHandle` detaches on drop — and a detached driver whose client
/// is gone keeps the socket open for as long as a response is still owed
/// (it sends `Terminate` only once nothing is pending), so a call cut
/// mid-statement by [`CALL_TIMEOUT`] or by the reconcile deadline would leave
/// its session open behind it, one more on every retry. Aborting closes the
/// socket with the call. What the server does with a statement already
/// running is then [`SESSION_OPTIONS`]' business: it is cancelled within
/// `statement_timeout` at most.
struct Driver(tokio::task::JoinHandle<Result<(), tokio_postgres::Error>>);

impl Driver {
    fn spawn<S, T>(connection: tokio_postgres::Connection<S, T>) -> Self
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
        T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        Self(tokio::spawn(connection))
    }

    /// Wait for the connection to finish on its own — after the client is
    /// dropped it sends `Terminate` and closes. Still aborted if THIS wait is
    /// cut, because `self` is dropped with it.
    async fn finish(mut self) -> Result<Result<(), tokio_postgres::Error>, tokio::task::JoinError> {
        (&mut self.0).await
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        // A no-op on a task that already finished.
        self.0.abort();
    }
}

/// Production [`PgAdmin`] over `tokio-postgres`.
///
/// No TLS, matching what an application's own connection does: the platform's
/// DSN builder (`cnpg::dsn`) emits no `sslmode` and every `needs.pg` workload
/// already connects that way to the same in-cluster service.
///
/// Every call is bounded three ways: the dial by [`CONNECT_TIMEOUT`], each
/// statement server-side by [`SESSION_OPTIONS`], and the whole call by
/// [`CALL_TIMEOUT`] — and a call that is abandoned, by its own bound or by
/// the caller, closes its connection ([`Driver`]).
pub struct PgClient;

impl PgClient {
    async fn execute_all_unbounded(
        host: &str,
        dsn: &str,
        statements: &[String],
    ) -> Result<(), PgAdminError> {
        let connect_err = |source| PgAdminError::Connect {
            host: host.to_string(),
            source,
        };
        let config = admin_config(dsn).map_err(connect_err)?;
        let (client, connection) = config
            .connect(tokio_postgres::NoTls)
            .await
            .map_err(connect_err)?;
        let driver = Driver::spawn(connection);

        let total = statements.len();
        let mut result = Ok(());
        for (index, statement) in statements.iter().enumerate() {
            if let Err(source) = client.batch_execute(statement).await {
                result = Err(statement_error(index, total, host, source));
                break;
            }
        }
        drop(client);
        // Awaited on BOTH paths: after a failed statement nothing is pending,
        // so the driver sends `Terminate` and ends at once — a close the
        // server logs as clean rather than as an unexpected EOF.
        let ended = driver.finish().await;
        // A connection that died mid-batch reports as a statement error above;
        // this catches the case where it died with nothing to attribute it to.
        if result.is_ok() {
            if let Ok(Err(_)) = ended {
                return Err(PgAdminError::ConnectionLost {
                    host: host.to_string(),
                });
            }
        }
        result
    }

    async fn extension_available_unbounded(
        host: &str,
        dsn: &str,
        extension: &str,
    ) -> Result<bool, PgAdminError> {
        let connect_err = |source| PgAdminError::Connect {
            host: host.to_string(),
            source,
        };
        let config = admin_config(dsn).map_err(connect_err)?;
        let (client, connection) = config
            .connect(tokio_postgres::NoTls)
            .await
            .map_err(connect_err)?;
        let driver = Driver::spawn(connection);
        // A QUERY, so this one CAN bind its parameter — and does. Only the
        // utility statements are forced to interpolate.
        let rows = client
            .query(crate::shared_pg::EXTENSION_AVAILABLE_QUERY, &[&extension])
            .await
            .map_err(|source| statement_error(0, 1, host, source));
        drop(client);
        let _ = driver.finish().await;
        Ok(!rows?.is_empty())
    }
}

#[async_trait]
impl PgAdmin for PgClient {
    async fn execute_all(&self, dsn: &str, statements: &[String]) -> Result<(), PgAdminError> {
        let host = host_of(dsn);
        bounded(&host, Self::execute_all_unbounded(&host, dsn, statements)).await
    }

    async fn extension_available(&self, dsn: &str, extension: &str) -> Result<bool, PgAdminError> {
        let host = host_of(dsn);
        bounded(
            &host,
            Self::extension_available_unbounded(&host, dsn, extension),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_error_names_the_host_and_never_the_dsn() {
        // The DSN carries the password. Naming the host is enough to locate
        // the failure and cannot leak one.
        assert_eq!(
            host_of("postgresql://role:sup3rs3cret@platform-postgres-rw.cnpg-system.svc:5432/db"),
            "platform-postgres-rw.cnpg-system.svc:5432"
        );
        assert!(!host_of("postgresql://r:sup3rs3cret@h:5432/db").contains("sup3rs3cret"));
    }

    #[test]
    fn host_of_degrades_rather_than_panicking_on_a_malformed_dsn() {
        // It feeds an error message; a panic here would replace a diagnosable
        // failure with an operator crash.
        assert_eq!(host_of("garbage"), "garbage");
        assert_eq!(host_of(""), "");
    }

    #[test]
    fn a_statement_error_identifies_the_statement_by_index_only() {
        // The Display impl is what reaches a log. A bind statement carries a
        // password, so the text must never be in it.
        let err = PgAdminError::ConnectionLost {
            host: "h:5432".into(),
        };
        assert!(format!("{err}").contains("h:5432"));
        // The Statement variant's own format string is asserted by shape here
        // rather than by constructing a tokio_postgres::Error, which has no
        // public constructor: the field set is what matters, and it holds no
        // statement text.
        fn _assert_no_statement_field(e: &PgAdminError) -> bool {
            matches!(
                e,
                PgAdminError::Statement {
                    index: _,
                    total: _,
                    host: _,
                    source: _
                } | PgAdminError::Connect { .. }
                    | PgAdminError::ConnectionLost { .. }
            )
        }
        assert!(_assert_no_statement_field(&err));
    }

    // --- probe_extensions: the three states must stay distinguishable ---

    /// A `PgAdmin` that answers from a fixed availability list, and can be
    /// told to fail the connection instead.
    struct FakePg {
        available: Vec<&'static str>,
        reachable: bool,
    }

    #[async_trait]
    impl PgAdmin for FakePg {
        async fn execute_all(&self, _dsn: &str, _s: &[String]) -> Result<(), PgAdminError> {
            Ok(())
        }
        async fn extension_available(
            &self,
            _dsn: &str,
            extension: &str,
        ) -> Result<bool, PgAdminError> {
            if !self.reachable {
                return Err(PgAdminError::ConnectionLost {
                    host: "h:5432".into(),
                });
            }
            Ok(self.available.contains(&extension))
        }
    }

    fn ext(name: &str) -> operator_core::PgExtension {
        operator_core::PgExtension {
            name: name.into(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn every_declared_extension_present_is_all_present() {
        let pg = FakePg {
            available: vec!["vector", "pg_trgm"],
            reachable: true,
        };
        let got = probe_extensions(&pg, "dsn", &[ext("vector"), ext("pg_trgm")]).await;
        assert_eq!(got, ExtensionProbe::AllPresent);
    }

    #[tokio::test]
    async fn declaring_nothing_is_all_present_not_a_probe() {
        let pg = FakePg {
            available: vec![],
            reachable: true,
        };
        assert_eq!(
            probe_extensions(&pg, "dsn", &[]).await,
            ExtensionProbe::AllPresent
        );
    }

    #[tokio::test]
    async fn an_absent_extension_is_reported_by_the_name_the_manifest_used() {
        let pg = FakePg {
            available: vec!["pg_trgm"],
            reachable: true,
        };
        let got = probe_extensions(&pg, "dsn", &[ext("vector"), ext("pg_trgm")]).await;
        assert_eq!(got, ExtensionProbe::Missing(vec!["vector".into()]));
    }

    #[tokio::test]
    async fn an_unreachable_server_is_not_an_empty_missing_list() {
        // THE distinction this enum exists for. Collapsing these two into
        // `Vec<String>` makes a network blip indistinguishable from a clean
        // result, and the caller then writes `Ready=True` on a database it
        // never actually asked about.
        let pg = FakePg {
            available: vec![],
            reachable: false,
        };
        let got = probe_extensions(&pg, "dsn", &[ext("vector")]).await;
        assert_eq!(got, ExtensionProbe::Unreachable);
        assert_ne!(got, ExtensionProbe::AllPresent);
    }

    // --- the bounds (WI-400): a real PgClient against a scripted server ---

    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::oneshot;

    /// What the scripted server does once a client has connected.
    #[derive(Clone, Copy)]
    enum Script {
        /// Read the startup packet, then never send a byte.
        Silent,
        /// Complete the startup handshake, then read whatever arrives and
        /// never answer it — a statement waiting on a lock, from the
        /// client's side of the socket.
        HangAfterStartup,
        /// Complete the startup handshake and answer every simple query with
        /// `CommandComplete`, except query `fail_at` (0-based), which fails
        /// with SQLSTATE `code`.
        FailQuery {
            fail_at: usize,
            code: &'static str,
            message: &'static str,
        },
    }

    /// What the scripted server observed.
    struct Seen {
        /// The startup parameters, as sent.
        startup: oneshot::Receiver<Vec<(String, String)>>,
        /// Fires when the client's side of the socket is closed.
        closed: oneshot::Receiver<()>,
    }

    /// A one-connection server speaking just enough of the PostgreSQL wire
    /// protocol (v3, no TLS: `PgClient` uses `NoTls`, so `sslmode=prefer`
    /// sends no SSLRequest) to drive the real `PgClient`. Returns the DSN to
    /// dial.
    async fn scripted_server(script: Script) -> (String, Seen) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let (startup_tx, startup) = oneshot::channel();
        let (closed_tx, closed) = oneshot::channel();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.expect("accept");
            let params = read_startup(&mut sock).await;
            let _ = startup_tx.send(params);
            match script {
                Script::Silent => {}
                Script::HangAfterStartup => send_ready(&mut sock).await,
                Script::FailQuery { .. } => send_ready(&mut sock).await,
            }
            let mut queries = 0usize;
            loop {
                let mut tag = [0u8; 1];
                if sock.read_exact(&mut tag).await.is_err() {
                    break;
                }
                let mut len = [0u8; 4];
                if sock.read_exact(&mut len).await.is_err() {
                    break;
                }
                let mut body = vec![0u8; (i32::from_be_bytes(len) as usize).saturating_sub(4)];
                if sock.read_exact(&mut body).await.is_err() {
                    break;
                }
                if let (
                    Script::FailQuery {
                        fail_at,
                        code,
                        message,
                    },
                    b'Q',
                ) = (script, tag[0])
                {
                    if queries == fail_at {
                        send_error(&mut sock, code, message).await;
                    } else {
                        send_command_complete(&mut sock).await;
                    }
                    queries += 1;
                }
            }
            let _ = closed_tx.send(());
        });
        (
            format!("postgresql://role:sup3rs3cret@127.0.0.1:{port}/db"),
            Seen { startup, closed },
        )
    }

    async fn read_startup(sock: &mut TcpStream) -> Vec<(String, String)> {
        let len = sock.read_i32().await.expect("startup length") as usize;
        let mut body = vec![0u8; len - 4];
        sock.read_exact(&mut body).await.expect("startup body");
        // body[0..4] is the protocol version; then key\0value\0 … \0.
        let fields: Vec<String> = body[4..]
            .split(|b| *b == 0)
            .map(|f| String::from_utf8_lossy(f).into_owned())
            .collect();
        fields
            .chunks(2)
            .filter(|kv| kv.len() == 2 && !kv[0].is_empty())
            .map(|kv| (kv[0].clone(), kv[1].clone()))
            .collect()
    }

    fn message(tag: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![tag];
        out.extend_from_slice(&((payload.len() + 4) as i32).to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    /// `AuthenticationOk` + `ReadyForQuery(idle)`.
    async fn send_ready(sock: &mut TcpStream) {
        let mut out = message(b'R', &0i32.to_be_bytes());
        out.extend(message(b'Z', b"I"));
        sock.write_all(&out).await.expect("write");
    }

    async fn send_command_complete(sock: &mut TcpStream) {
        let mut out = message(b'C', b"DO\0");
        out.extend(message(b'Z', b"I"));
        sock.write_all(&out).await.expect("write");
    }

    async fn send_error(sock: &mut TcpStream, code: &str, text: &str) {
        let mut fields = Vec::new();
        for (field, value) in [(b'S', "ERROR"), (b'V', "ERROR"), (b'C', code), (b'M', text)] {
            fields.push(field);
            fields.extend_from_slice(value.as_bytes());
            fields.push(0);
        }
        fields.push(0);
        let mut out = message(b'E', &fields);
        out.extend(message(b'Z', b"I"));
        sock.write_all(&out).await.expect("write");
    }

    fn statements(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("SELECT {i}")).collect()
    }

    #[test]
    fn the_admin_config_bounds_the_dial_and_carries_the_session_bounds() {
        let config = admin_config("postgresql://r:pw@platform-postgres-rw.cnpg-system.svc:5432/db")
            .expect("a well-formed DSN parses");
        assert_eq!(config.get_connect_timeout(), Some(&CONNECT_TIMEOUT));
        assert_eq!(config.get_options(), Some(SESSION_OPTIONS));
    }

    #[test]
    fn the_server_side_bounds_stay_inside_the_call_bound() {
        // The server-side bounds must fire first on a server that answers, so
        // the caller learns WHAT held it up; the call bound is for a server
        // that does not answer at all.
        assert!(CONNECT_TIMEOUT + Duration::from_secs(30) < CALL_TIMEOUT);
        assert!(SESSION_OPTIONS.contains("lock_timeout=10s"));
        assert!(SESSION_OPTIONS.contains("statement_timeout=30s"));
    }

    #[tokio::test]
    async fn every_session_asks_the_server_for_the_lock_and_statement_bounds() {
        let (dsn, seen) = scripted_server(Script::FailQuery {
            fail_at: usize::MAX,
            code: "",
            message: "",
        })
        .await;
        PgClient
            .execute_all(&dsn, &statements(2))
            .await
            .expect("a batch the server completes is Ok");
        let startup = seen.startup.await.expect("startup seen");
        assert!(
            startup.contains(&("options".to_string(), SESSION_OPTIONS.to_string())),
            "{startup:?}"
        );
        assert!(startup.contains(&("user".to_string(), "role".to_string())));
        assert!(startup.contains(&("database".to_string(), "db".to_string())));
    }

    #[tokio::test]
    async fn a_lock_timeout_is_the_server_cancelling_the_statement() {
        let (dsn, _seen) = scripted_server(Script::FailQuery {
            fail_at: 1,
            code: "55P03",
            message: "canceling statement due to lock timeout",
        })
        .await;
        let err = PgClient
            .execute_all(&dsn, &statements(3))
            .await
            .expect_err("statement #1 fails");
        match &err {
            PgAdminError::ServerCancelled {
                index,
                total,
                host,
                bound,
                cause,
            } => {
                assert_eq!((*index, *total), (1, 3));
                assert!(host.starts_with("127.0.0.1:"), "{host}");
                assert_eq!(*bound, ServerBound::Lock);
                assert_eq!(cause, "canceling statement due to lock timeout");
            }
            other => panic!("expected ServerCancelled, got {other:?}"),
        }
        // The message an operator reads names the cause and never the DSN.
        assert!(err.to_string().contains("lock timeout"), "{err}");
        assert!(!err.to_string().contains("sup3rs3cret"), "{err}");
    }

    #[tokio::test]
    async fn a_statement_timeout_is_the_server_cancelling_the_statement() {
        let (dsn, _seen) = scripted_server(Script::FailQuery {
            fail_at: 0,
            code: "57014",
            message: "canceling statement due to statement timeout",
        })
        .await;
        let err = PgClient
            .execute_all(&dsn, &statements(1))
            .await
            .expect_err("statement #0 fails");
        assert!(
            matches!(
                err,
                PgAdminError::ServerCancelled {
                    index: 0,
                    total: 1,
                    bound: ServerBound::Statement,
                    ..
                }
            ),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn any_other_server_error_stays_a_statement_error() {
        // A refused GRANT is not a timeout, and must not be reported as one.
        let (dsn, _seen) = scripted_server(Script::FailQuery {
            fail_at: 0,
            code: "42501",
            message: "permission denied to grant role",
        })
        .await;
        let err = PgClient
            .execute_all(&dsn, &statements(2))
            .await
            .expect_err("statement #0 fails");
        assert!(
            matches!(
                err,
                PgAdminError::Statement {
                    index: 0,
                    total: 2,
                    ..
                }
            ),
            "{err:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_server_that_never_answers_is_given_up_on_at_the_call_bound() {
        // Accepts the dial, then says nothing: `connect_timeout` does not
        // cover the startup handshake, so before WI-400 this call never
        // returned. Bounded from outside as well, so a missing bound fails
        // the test instead of hanging it.
        let (dsn, _seen) = scripted_server(Script::Silent).await;
        let started = tokio::time::Instant::now();
        let got =
            tokio::time::timeout(CALL_TIMEOUT * 2, PgClient.execute_all(&dsn, &statements(1)))
                .await
                .expect("execute_all must give up on its own");
        assert!(
            matches!(&got, Err(PgAdminError::TimedOut { after, .. }) if *after == CALL_TIMEOUT),
            "{got:?}"
        );
        assert_eq!(started.elapsed(), CALL_TIMEOUT);
    }

    #[tokio::test(start_paused = true)]
    async fn an_extension_probe_against_a_silent_server_is_bounded_too() {
        let (dsn, _seen) = scripted_server(Script::Silent).await;
        let got = tokio::time::timeout(
            CALL_TIMEOUT * 2,
            PgClient.extension_available(&dsn, "vector"),
        )
        .await
        .expect("extension_available must give up on its own");
        assert!(matches!(got, Err(PgAdminError::TimedOut { .. })), "{got:?}");
    }

    #[tokio::test]
    async fn an_abandoned_batch_closes_its_connection() {
        // The reconcile deadline drops a call mid-statement. Its socket must
        // close with it: a detached driver keeps it open for as long as a
        // response is owed, and every retry would add one more session.
        let (dsn, seen) = scripted_server(Script::HangAfterStartup).await;
        let cut = tokio::time::timeout(
            Duration::from_millis(300),
            PgClient.execute_all(&dsn, &statements(1)),
        )
        .await;
        assert!(cut.is_err(), "the scripted server never answers: {cut:?}");
        tokio::time::timeout(Duration::from_secs(5), seen.closed)
            .await
            .expect("the connection must close when its call is dropped")
            .expect("the server task reports the close");
    }

    #[tokio::test]
    async fn an_abandoned_extension_probe_closes_its_connection() {
        let (dsn, seen) = scripted_server(Script::HangAfterStartup).await;
        let cut = tokio::time::timeout(
            Duration::from_millis(300),
            PgClient.extension_available(&dsn, "vector"),
        )
        .await;
        assert!(cut.is_err(), "the scripted server never answers: {cut:?}");
        tokio::time::timeout(Duration::from_secs(5), seen.closed)
            .await
            .expect("the connection must close when its call is dropped")
            .expect("the server task reports the close");
    }
}
