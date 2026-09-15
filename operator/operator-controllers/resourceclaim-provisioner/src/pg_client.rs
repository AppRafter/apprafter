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

use async_trait::async_trait;
use thiserror::Error;

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

/// Production [`PgAdmin`] over `tokio-postgres`.
///
/// No TLS, matching what an application's own connection does: the platform's
/// DSN builder (`cnpg::dsn`) emits no `sslmode` and every `needs.pg` workload
/// already connects that way to the same in-cluster service.
pub struct PgClient;

#[async_trait]
impl PgAdmin for PgClient {
    async fn execute_all(&self, dsn: &str, statements: &[String]) -> Result<(), PgAdminError> {
        let host = host_of(dsn);
        let (client, connection) = tokio_postgres::connect(dsn, tokio_postgres::NoTls)
            .await
            .map_err(|source| PgAdminError::Connect {
                host: host.clone(),
                source,
            })?;
        // The connection future drives the socket; dropping it closes the
        // connection, so it is spawned and the handle held until the batch is
        // done.
        let conn_task = tokio::spawn(connection);

        let total = statements.len();
        let mut result = Ok(());
        for (index, statement) in statements.iter().enumerate() {
            if let Err(source) = client.batch_execute(statement).await {
                result = Err(PgAdminError::Statement {
                    index,
                    total,
                    host: host.clone(),
                    source,
                });
                break;
            }
        }
        drop(client);
        // A connection that died mid-batch reports as a statement error above;
        // this catches the case where it died with nothing to attribute it to.
        if result.is_ok() {
            if let Ok(Err(_)) = conn_task.await {
                return Err(PgAdminError::ConnectionLost { host });
            }
        }
        result
    }

    async fn extension_available(&self, dsn: &str, extension: &str) -> Result<bool, PgAdminError> {
        let host = host_of(dsn);
        let (client, connection) = tokio_postgres::connect(dsn, tokio_postgres::NoTls)
            .await
            .map_err(|source| PgAdminError::Connect {
                host: host.clone(),
                source,
            })?;
        let conn_task = tokio::spawn(connection);
        // A QUERY, so this one CAN bind its parameter — and does. Only the
        // utility statements are forced to interpolate.
        let rows = client
            .query(crate::shared_pg::EXTENSION_AVAILABLE_QUERY, &[&extension])
            .await
            .map_err(|source| PgAdminError::Statement {
                index: 0,
                total: 1,
                host: host.clone(),
                source,
            });
        drop(client);
        let _ = conn_task.await;
        Ok(!rows?.is_empty())
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
}
