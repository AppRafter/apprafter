// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Does the Postgres admin client actually drive the shared-database SQL?
//! (2.29 / ADR 0066 §3.1)
//!
//! Gated like the other real-backend smoke tests in this repository: env var
//! plus `--ignored`, so an ordinary `cargo test` never needs a server.
//!
//! ```text
//! APPRAFTER_PG_SMOKE_DSN='postgresql://postgres@127.0.0.1:15432/postgres' \
//!   cargo test -p operator-controllers-resourceclaim-provisioner \
//!   --test pg_client_smoke_test -- --ignored --nocapture
//! ```
//!
//! `e2e/shared-pg-sql-check.sh` runs it with a throwaway container.
//!
//! WHAT THIS ADDS over the shell check. The shell check proves the STATEMENTS
//! work by piping them through `psql`. This proves the CLIENT works: that
//! `execute_all` sequences a batch the way `psql` does, that a failure is
//! reported against the right statement index, and that `extension_available`
//! answers off the live catalogue. Those are properties of the Rust code, and
//! `psql` cannot observe them.

use operator_controllers_resourceclaim_provisioner::pg_client::{PgAdmin, PgClient};
use operator_controllers_resourceclaim_provisioner::shared_pg::{
    bind_consumer, create_groups, grant_reader, quote_ident, shared_group, Access,
};

fn dsn() -> Option<String> {
    std::env::var("APPRAFTER_PG_SMOKE_DSN")
        .ok()
        .filter(|s| !s.is_empty())
}

/// A DSN for `db` on the same server as the admin DSN.
fn dsn_for(admin: &str, db: &str) -> String {
    match admin.rsplit_once('/') {
        Some((prefix, _)) => format!("{prefix}/{db}"),
        None => admin.to_string(),
    }
}

#[tokio::test]
#[ignore]
async fn the_client_drives_the_whole_shared_database_sequence() {
    let Some(admin_dsn) = dsn() else {
        panic!("APPRAFTER_PG_SMOKE_DSN is unset — this test would have judged nothing");
    };
    let client = PgClient;
    let ns = "apps";
    let shared = "clientcheck";
    let db = shared_group(ns, shared);

    // 1. The platform role, as CNPG's managed.roles would create it. Not a
    //    superuser: that is the arrangement under test.
    client
        .execute_all(
            &admin_dsn,
            &[
                "DO $apprafter$ BEGIN IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = \
                 'apprafter_admin') THEN CREATE ROLE apprafter_admin LOGIN PASSWORD 'a' \
                 CREATEROLE CREATEDB; END IF; END $apprafter$;"
                    .to_string(),
            ],
        )
        .await
        .expect("creating the platform role");

    // 2. Groups, through the builders; then the database, which CANNOT be
    //    guarded by a DO block (CREATE DATABASE does not run inside one), so
    //    the reconciler checks `pg_database` itself and this test tolerates a
    //    re-run instead.
    client
        .execute_all(&admin_dsn, &create_groups(ns, shared, "apprafter_admin"))
        .await
        .expect("group creation");
    let _ = client
        .execute_all(
            &admin_dsn,
            &[format!(
                "CREATE DATABASE {} OWNER {};",
                quote_ident(&db),
                quote_ident(&shared_group(ns, shared))
            )],
        )
        .await;

    // 3. Reader grants + one rw and one ro consumer, in the shared database.
    let db_dsn = dsn_for(&admin_dsn, &db);
    let mut in_db = grant_reader(ns, shared, &db);
    in_db.extend(bind_consumer(
        ns,
        shared,
        "claim_apps_rw_pg",
        &db,
        "p'w$$q",
        Access::ReadWrite,
    ));
    in_db.extend(bind_consumer(
        ns,
        shared,
        "claim_apps_ro_pg",
        &db,
        "p'w$$q",
        Access::ReadOnly,
    ));
    client
        .execute_all(&db_dsn, &in_db)
        .await
        .expect("in-database batch");

    // 4. `extension_available` answers off the live catalogue, both ways.
    assert!(
        client
            .extension_available(&db_dsn, "plpgsql")
            .await
            .expect("query"),
        "plpgsql is in every PostgreSQL"
    );
    assert!(
        !client
            .extension_available(&db_dsn, "definitely_not_an_extension")
            .await
            .expect("query"),
        "an absent extension must read false, not error"
    );

    // 5. A failing statement is attributed to its INDEX, and the message
    //    carries no statement text — which matters because a bind statement
    //    in the same batch shape carries a password.
    let err = client
        .execute_all(
            &db_dsn,
            &[
                "SELECT 1;".to_string(),
                "THIS IS NOT SQL;".to_string(),
                "SELECT 2;".to_string(),
            ],
        )
        .await
        .expect_err("a malformed statement must fail the batch");
    let rendered = format!("{err}");
    assert!(
        rendered.contains("#1 of 3"),
        "the failure must name the statement's index: {rendered}"
    );
    assert!(
        !rendered.contains("THIS IS NOT SQL"),
        "the statement text must never reach the error: {rendered}"
    );
}
