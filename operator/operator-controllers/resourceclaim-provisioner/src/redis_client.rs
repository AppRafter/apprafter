// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Imperative Redis I/O for per-claim ACL management (ADR 0042 §2/§4).
//!
//! Per-claim `$N`-pinned ACL users are *runtime* state on a Dragonfly
//! instance (lost when the pod restarts), so they cannot be declared on
//! the `Dragonfly` CR — the provisioner drives them imperatively over a
//! Redis connection as the instance admin user. This module is the thin
//! I/O seam: a [`RedisAdmin`] trait (mirrors `oci_resolve::RegistryHttp`
//! and `grace`'s injected clock) so the reconcile + GC logic is unit-
//! testable with a fake, and a production [`RedisClient`] over the
//! `redis` crate's async multiplexed connection.
//!
//! Error messages are deliberately generic — they NEVER interpolate the
//! admin password or a claim password, so a logged `RedisAdminError`
//! cannot leak a credential.

use async_trait::async_trait;
use thiserror::Error;

/// Errors from the imperative Redis admin path. Generic by construction:
/// no variant carries a password, so logging one is leak-safe.
#[derive(Debug, Error)]
pub enum RedisAdminError {
    /// Could not open a connection to the instance (DNS, dial, auth).
    #[error("redis connect to {addr} failed: {source}")]
    Connect {
        addr: String,
        #[source]
        source: redis::RedisError,
    },
    /// A command (`ACL SETUSER` / `ACL DELUSER` / `SELECT` / `FLUSHDB` /
    /// `DBSIZE`) failed on the wire. The command verb is named; arguments
    /// (which include passwords for `SETUSER`) are NOT.
    #[error("redis command {verb} failed on {addr}: {source}")]
    Command {
        verb: &'static str,
        addr: String,
        #[source]
        source: redis::RedisError,
    },
}

/// Imperative Redis admin operations the provisioner + GC drive against a
/// shared pool instance, authenticated as the instance admin user. All are
/// idempotent (`dbsize` trivially so, being a read) — re-reconciles and
/// re-pins are safe.
#[async_trait]
pub trait RedisAdmin: Send + Sync {
    /// Run `ACL SETUSER <args...>` on `addr` as the admin user. `args` is
    /// the vector from [`crate::dragonfly::acl_setuser_args`] (the first
    /// element is the username; the rest are the rule tokens). Idempotent:
    /// SETUSER upserts, and the rule vector carries `resetkeys` /
    /// `resetchannels` so a re-apply is byte-stable.
    async fn acl_setuser(
        &self,
        addr: &str,
        admin_pw: &str,
        args: &[String],
    ) -> Result<(), RedisAdminError>;

    /// Run `ACL DELUSER <user>` on `addr` (idempotent — DELUSER of a
    /// missing user returns `0`, which is Ok here).
    async fn acl_deluser(
        &self,
        addr: &str,
        admin_pw: &str,
        user: &str,
    ) -> Result<(), RedisAdminError>;

    /// `SELECT <dbnum>; FLUSHDB ASYNC` on `addr` (recycle-safety on
    /// allocate + reclaim on GC — ADR 0042 §3/§8). Idempotent: flushing an
    /// empty DB is a no-op.
    async fn flushdb(&self, addr: &str, admin_pw: &str, dbnum: u16) -> Result<(), RedisAdminError>;

    /// `SELECT <dbnum>; DBSIZE` on `addr` — the number of keys in one
    /// tenant's logical DB (2.22d / D8).
    ///
    /// A COUNT, not bytes, and every caller must render it as such. That is
    /// not a shortcut taken for convenience; it is the whole of what
    /// Dragonfly will tell us about one `$N`, established by reading the
    /// v1.37.0 source rather than assumed:
    ///
    ///  * per-DB byte figures DO exist — `obj_memory_usage` and
    ///    `table_mem_usage` live per database in `Metrics.db_stats[i]`, and
    ///    are still in scope at the very loop that emits the `db`-labelled
    ///    metrics — but every emission site sums across databases before
    ///    printing, so nothing reaches a client per-DB;
    ///  * the seven `db`-labelled Prometheus metrics are all counters;
    ///  * several `DEBUG` subcommands (`SEGMENTS`, `VALUES`, `TOPK`, `KEYS`,
    ///    `COMPRESSION`) genuinely ARE scoped to the selected DB, but report
    ///    slot capacity, logical lengths or a truncated Huffman-training
    ///    sample — none is a byte total, and `COMPRESSION`'s `raw_size` is
    ///    the dangerous one, because it looks like a size and is not;
    ///  * `INFO MEMORY` is scoped to the caller's dragonfly NAMESPACE, which
    ///    would work, but on this version a non-default namespace is not
    ///    snapshotted, not exported, and — because the replication journal
    ///    carries no namespace — is replayed into the replica's default
    ///    namespace. That route costs data, not just accuracy.
    ///
    /// So the honest per-tenant figure is the key count, and the CLI labels
    /// it `keys`. Bytes would need either a ~3-line upstream patch at the
    /// existing per-db emission loop, or one Dragonfly instance per claim.
    async fn dbsize(&self, addr: &str, admin_pw: &str, dbnum: u16) -> Result<i64, RedisAdminError>;
}

/// Production [`RedisAdmin`] over the `redis` crate's async multiplexed
/// connection. Connects fresh per call (these are infrequent control-
/// plane ops, not a hot data path) as `default:<admin_pw>` over the
/// instance's in-cluster Service.
#[derive(Default)]
pub struct RedisClient;

impl RedisClient {
    /// Open an authenticated multiplexed async connection to `addr`
    /// (`host:port`, no scheme) as the `default` admin user.
    async fn connect(
        &self,
        addr: &str,
        admin_pw: &str,
    ) -> Result<redis::aio::MultiplexedConnection, RedisAdminError> {
        // The admin user on a Dragonfly instance is `default`; the
        // password comes from the per-instance admin Secret. Building the
        // `ConnectionInfo` directly keeps the password out of any URL
        // string (and thus out of any error rendering).
        let conn_info = redis::ConnectionInfo {
            addr: parse_host_port(addr),
            redis: redis::RedisConnectionInfo {
                db: 0,
                username: None,
                password: Some(admin_pw.to_string()),
                protocol: redis::ProtocolVersion::RESP2,
            },
        };
        let client = redis::Client::open(conn_info).map_err(|source| RedisAdminError::Connect {
            addr: addr.to_string(),
            source,
        })?;
        client
            .get_multiplexed_async_connection()
            .await
            .map_err(|source| RedisAdminError::Connect {
                addr: addr.to_string(),
                source,
            })
    }
}

/// Split a `host:port` address into the `redis` crate's `ConnectionAddr`.
/// A missing/garbage port falls back to 6379 (the Dragonfly default).
fn parse_host_port(addr: &str) -> redis::ConnectionAddr {
    match addr.rsplit_once(':') {
        Some((host, port)) => {
            redis::ConnectionAddr::Tcp(host.to_string(), port.parse().unwrap_or(6379))
        }
        None => redis::ConnectionAddr::Tcp(addr.to_string(), 6379),
    }
}

#[async_trait]
impl RedisAdmin for RedisClient {
    async fn acl_setuser(
        &self,
        addr: &str,
        admin_pw: &str,
        args: &[String],
    ) -> Result<(), RedisAdminError> {
        let mut conn = self.connect(addr, admin_pw).await?;
        let mut cmd = redis::cmd("ACL");
        cmd.arg("SETUSER");
        for a in args {
            cmd.arg(a);
        }
        cmd.query_async::<()>(&mut conn)
            .await
            .map_err(|source| RedisAdminError::Command {
                verb: "ACL SETUSER",
                addr: addr.to_string(),
                source,
            })
    }

    async fn acl_deluser(
        &self,
        addr: &str,
        admin_pw: &str,
        user: &str,
    ) -> Result<(), RedisAdminError> {
        let mut conn = self.connect(addr, admin_pw).await?;
        // DELUSER returns the count removed (0 if absent) — discard it;
        // a missing user is success here.
        redis::cmd("ACL")
            .arg("DELUSER")
            .arg(user)
            .query_async::<i64>(&mut conn)
            .await
            .map(|_| ())
            .map_err(|source| RedisAdminError::Command {
                verb: "ACL DELUSER",
                addr: addr.to_string(),
                source,
            })
    }

    async fn flushdb(&self, addr: &str, admin_pw: &str, dbnum: u16) -> Result<(), RedisAdminError> {
        let mut conn = self.connect(addr, admin_pw).await?;
        redis::cmd("SELECT")
            .arg(dbnum)
            .query_async::<()>(&mut conn)
            .await
            .map_err(|source| RedisAdminError::Command {
                verb: "SELECT",
                addr: addr.to_string(),
                source,
            })?;
        redis::cmd("FLUSHDB")
            .arg("ASYNC")
            .query_async::<()>(&mut conn)
            .await
            .map_err(|source| RedisAdminError::Command {
                verb: "FLUSHDB",
                addr: addr.to_string(),
                source,
            })
    }

    async fn dbsize(&self, addr: &str, admin_pw: &str, dbnum: u16) -> Result<i64, RedisAdminError> {
        let mut conn = self.connect(addr, admin_pw).await?;
        redis::cmd("SELECT")
            .arg(dbnum)
            .query_async::<()>(&mut conn)
            .await
            .map_err(|source| RedisAdminError::Command {
                verb: "SELECT",
                addr: addr.to_string(),
                source,
            })?;
        redis::cmd("DBSIZE")
            .query_async::<i64>(&mut conn)
            .await
            .map_err(|source| RedisAdminError::Command {
                verb: "DBSIZE",
                addr: addr.to_string(),
                source,
            })
    }
}

/// Test double for [`RedisAdmin`] that records every call instead of
/// touching a real instance. Lets the reconcile + GC dragonfly paths be
/// driven in unit tests without a live Dragonfly (the trait-seam payoff;
/// mirrors how `oci_resolve` is tested with a fake `RegistryHttp`).
/// `#[cfg(test)]` — compiled only for this crate's own unit tests.
#[cfg(test)]
#[derive(Default)]
pub struct FakeRedis {
    pub setuser_calls: std::sync::Mutex<Vec<(String, Vec<String>)>>,
    pub deluser_calls: std::sync::Mutex<Vec<(String, String)>>,
    pub flushdb_calls: std::sync::Mutex<Vec<(String, u16)>>,
    pub dbsize_calls: std::sync::Mutex<Vec<(String, u16)>>,
    /// What [`RedisAdmin::dbsize`] answers. `None` makes the call FAIL,
    /// so a test can drive the unreachable-instance path — the one that
    /// must leave the previous figure alone rather than stamping a zero.
    pub dbsize_answer: std::sync::Mutex<Option<i64>>,
}

#[cfg(test)]
#[async_trait]
impl RedisAdmin for FakeRedis {
    async fn acl_setuser(
        &self,
        addr: &str,
        _admin_pw: &str,
        args: &[String],
    ) -> Result<(), RedisAdminError> {
        self.setuser_calls
            .lock()
            .unwrap()
            .push((addr.to_string(), args.to_vec()));
        Ok(())
    }

    async fn acl_deluser(
        &self,
        addr: &str,
        _admin_pw: &str,
        user: &str,
    ) -> Result<(), RedisAdminError> {
        self.deluser_calls
            .lock()
            .unwrap()
            .push((addr.to_string(), user.to_string()));
        Ok(())
    }

    async fn flushdb(
        &self,
        addr: &str,
        _admin_pw: &str,
        dbnum: u16,
    ) -> Result<(), RedisAdminError> {
        self.flushdb_calls
            .lock()
            .unwrap()
            .push((addr.to_string(), dbnum));
        Ok(())
    }

    async fn dbsize(
        &self,
        addr: &str,
        _admin_pw: &str,
        dbnum: u16,
    ) -> Result<i64, RedisAdminError> {
        self.dbsize_calls
            .lock()
            .unwrap()
            .push((addr.to_string(), dbnum));
        self.dbsize_answer
            .lock()
            .unwrap()
            .ok_or_else(|| RedisAdminError::Command {
                verb: "DBSIZE",
                addr: addr.to_string(),
                source: redis::RedisError::from((redis::ErrorKind::IoError, "fake instance down")),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_unreachable_instance_fails_dbsize_rather_than_answering_zero() {
        // The default answer is absent, and absent must be an ERROR, not a
        // zero. A zero would be written to the claim as "this tenant holds
        // nothing" — a confident statement about someone's data, made
        // because we could not reach the server.
        let fake = FakeRedis::default();
        assert!(fake.dbsize("h:6379", "pw", 7).await.is_err());
    }

    #[tokio::test]
    async fn fake_redis_records_each_call() {
        let fake = FakeRedis::default();
        fake.flushdb("h:6379", "pw", 7).await.unwrap();
        fake.acl_setuser("h:6379", "pw", &["u".into(), "on".into()])
            .await
            .unwrap();
        fake.acl_deluser("h:6379", "pw", "u").await.unwrap();
        *fake.dbsize_answer.lock().unwrap() = Some(4200);
        assert_eq!(fake.dbsize("h:6379", "pw", 7).await.unwrap(), 4200);
        assert_eq!(
            fake.dbsize_calls.lock().unwrap().as_slice(),
            &[("h:6379".to_string(), 7)]
        );
        assert_eq!(
            fake.flushdb_calls.lock().unwrap().as_slice(),
            &[("h:6379".to_string(), 7)]
        );
        assert_eq!(
            fake.setuser_calls.lock().unwrap()[0],
            (
                "h:6379".to_string(),
                vec!["u".to_string(), "on".to_string()]
            )
        );
        assert_eq!(
            fake.deluser_calls.lock().unwrap().as_slice(),
            &[("h:6379".to_string(), "u".to_string())]
        );
    }

    #[test]
    fn parse_host_port_splits_service_addr() {
        let a = parse_host_port("platform-redis-ephemeral-000.dragonfly-system.svc:6379");
        assert_eq!(
            a,
            redis::ConnectionAddr::Tcp(
                "platform-redis-ephemeral-000.dragonfly-system.svc".into(),
                6379
            )
        );
    }

    #[test]
    fn parse_host_port_defaults_port_when_missing() {
        let a = parse_host_port("some-host");
        assert_eq!(a, redis::ConnectionAddr::Tcp("some-host".into(), 6379));
    }

    #[test]
    fn error_messages_never_carry_passwords() {
        // The Command/Connect variants render the addr + verb only — a
        // sanity guard that no password field exists to leak.
        let err = RedisAdminError::Command {
            verb: "ACL SETUSER",
            addr: "h:6379".into(),
            source: redis::RedisError::from((redis::ErrorKind::AuthenticationFailed, "x")),
        };
        let rendered = format!("{err}");
        assert!(rendered.contains("ACL SETUSER"));
        assert!(rendered.contains("h:6379"));
    }
}
