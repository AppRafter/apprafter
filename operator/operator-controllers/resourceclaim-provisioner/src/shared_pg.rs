// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The SQL a shared Postgres database is built from (2.29 / ADR 0066 §3.1).
//!
//! Every function here is pure — it returns statement STRINGS — so the whole
//! role model is unit-testable without a server, the same shape `cnpg.rs` and
//! `dragonfly.rs` already use for their CR builders.
//!
//! # Why the platform executes SQL at all
//!
//! CNPG's declarative `managed.roles` creates exactly ONE role here, the
//! platform's own `apprafter_admin`. It cannot create the rest, and that was
//! MEASURED rather than assumed
//! (`docs/measurements/2.29-shared-database-2026-09-15.md`): a `CREATEROLE`
//! role without superuser may administer only the roles it holds ADMIN OPTION
//! on, and it obtains ADMIN OPTION by CREATING them. With the groups created
//! by somebody else, both of the statements this design needs are refused:
//!
//! ```text
//! ALTER ROLE <consumer> IN DATABASE <db> SET ROLE <group>;
//!   ERROR: permission denied to alter role
//!   DETAIL: Only roles with the CREATEROLE attribute and the ADMIN option
//!           on role "<consumer>" may alter this role.
//!
//! CREATE ROLE <consumer> LOGIN IN ROLE <group>;
//!   ERROR: permission denied to grant role "<group>"
//!   DETAIL: Only roles with the ADMIN option on role "<group>" may grant it.
//! ```
//!
//! So the platform role creates the groups, grants itself membership in them
//! WITH SET (owning a database as a group needs membership, not merely
//! admin), and creates the consumer roles.
//!
//! # Why `SET ROLE` is the whole design
//!
//! Without `ALTER ROLE <consumer> IN DATABASE <db> SET ROLE <group>`, a table
//! created by one consumer's migration is owned by THAT consumer and the next
//! one cannot read it — the classic shared-database failure, which surfaces as
//! "my neighbour's migration broke my app" rather than as a permission error
//! anyone recognises. With it, every object any consumer creates belongs to
//! the group, so membership alone suffices and a single
//! `ALTER DEFAULT PRIVILEGES FOR ROLE <group>` covers every future table no
//! matter who creates it. Measured end to end.
//!
//! # Quoting, and why the password IS interpolated
//!
//! Identifiers are composed by [`crate::cnpg::pg_identifier`] and
//! [`shared_group`], whose alphabet is `[a-z0-9_]` — so they cannot carry a
//! quote, a semicolon or a space. [`quote_ident`] double-quotes them anyway:
//! the cost is nothing and the alternative is a rule that holds until someone
//! widens a fold.
//!
//! **The password is interpolated through [`quote_literal`], and that is
//! forced rather than chosen.** An earlier version of this module emitted
//! `PASSWORD $1` and documented that the caller would bind it. Measured on
//! PostgreSQL 18.4, neither half of that works:
//!
//! ```text
//! DO $$ BEGIN CREATE ROLE zz LOGIN PASSWORD $1; END $$;
//!   ERROR: syntax error at or near "$1"      -- a DO block takes no parameters
//!
//! PREPARE p AS CREATE ROLE yy LOGIN PASSWORD $1;
//!   ERROR: syntax error at or near "CREATE"  -- a utility statement cannot be prepared
//! ```
//!
//! So `CREATE ROLE` / `ALTER ROLE` cannot be parameterised at all, by any
//! client. What remains is correct literal quoting, and the DO block is
//! dollar-quoted with the distinguishable tag `$apprafter$` so a password
//! containing `$$` cannot terminate the body. Both are verified against a real
//! server by `e2e/shared-pg-sql-check.sh`, which executes THESE builders'
//! output rather than a retyped copy of it.
//!
//! The consequence for logging is real and belongs here: a statement string
//! from `bind_consumer` CONTAINS a credential and must never be logged. The
//! caller logs the statement INDEX, not its text.

use crate::cnpg::pg_identifier;

/// The `NOLOGIN` group that OWNS a shared database. Every object any consumer
/// creates ends up owned by this role, which is what makes the database
/// shareable at all.
pub fn shared_group(namespace: &str, name: &str) -> String {
    // `shd_` rather than `claim_`: a shared database is not a claim, and the
    // two must not be able to collide in the role namespace.
    let folded = pg_identifier(namespace, name);
    folded.replacen("claim_", "shd_", 1)
}

/// The `NOLOGIN` group that holds SELECT on a shared database, for `ro`
/// consumers.
pub fn shared_reader_group(namespace: &str, name: &str) -> String {
    format!("{}_ro", shared_group(namespace, name))
}

/// Double-quote a Postgres identifier. The inputs are already restricted to
/// `[a-z0-9_]` by their own derivation, so this doubles no quotes in practice
/// — it is here so that a future widening of that alphabet cannot turn a
/// composed name into executable syntax.
pub fn quote_ident(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

/// Quote a string LITERAL. `standard_conforming_strings` is on by default, so
/// doubling the single quote is the whole rule and a backslash is an ordinary
/// character.
///
/// Used for one thing: the generated password, which cannot be bound as a
/// parameter because `CREATE ROLE` is a utility statement (measured — see this
/// module's own header).
pub fn quote_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Statements that create a shared database's two groups and give the
/// platform role what it needs to administer them.
///
/// Idempotent by construction is NOT available — Postgres has no
/// `CREATE ROLE IF NOT EXISTS` — so each statement is wrapped in a `DO` block
/// that checks `pg_roles` first. The caller may therefore re-run the whole
/// list on every reconcile, which is what makes this safe to drive from a
/// controller rather than from a one-shot job.
pub fn create_groups(namespace: &str, name: &str, platform_role: &str) -> Vec<String> {
    let owner = shared_group(namespace, name);
    let reader = shared_reader_group(namespace, name);
    let mut out = Vec::new();
    for group in [&owner, &reader] {
        out.push(format!(
            "DO $apprafter$ BEGIN IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = {}) \
             THEN CREATE ROLE {} NOLOGIN; END IF; END $apprafter$;",
            quote_literal(group),
            quote_ident(group)
        ));
        // Membership WITH SET, not merely the ADMIN OPTION creation already
        // conferred: owning a database as this group requires the creator to
        // be able to SET ROLE to it (measured — `must be able to SET ROLE`).
        out.push(format!(
            "GRANT {} TO {} WITH SET TRUE;",
            quote_ident(group),
            quote_ident(platform_role)
        ));
    }
    out
}

/// Which privilege level a consumer binds at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    ReadWrite,
    ReadOnly,
}

impl Access {
    /// Parse the claim's `spec.access`. Anything unrecognised is READ-ONLY,
    /// deliberately: the webhook constrains the field to `rw`/`ro`, so an
    /// unexpected value means something upstream is wrong, and the safe
    /// reading of "something is wrong" is the lesser privilege.
    pub fn from_spec(access: Option<&str>) -> Self {
        match access {
            Some("ro") => Access::ReadOnly,
            Some("rw") | None => Access::ReadWrite,
            Some(_) => Access::ReadOnly,
        }
    }
}

/// Statements that bind ONE consumer to a shared database.
///
/// **The returned strings contain the password.** They cannot not: `CREATE
/// ROLE` is a utility statement and PostgreSQL will not prepare or parameterise
/// one (measured — see this module's header). The caller must therefore never
/// log a statement from this function; log its index.
pub fn bind_consumer(
    namespace: &str,
    shared_name: &str,
    consumer_role: &str,
    database: &str,
    password: &str,
    access: Access,
) -> Vec<String> {
    let owner = shared_group(namespace, shared_name);
    let reader = shared_reader_group(namespace, shared_name);
    let group = match access {
        Access::ReadWrite => &owner,
        Access::ReadOnly => &reader,
    };
    let role_q = quote_ident(consumer_role);

    let pw = quote_literal(password);
    let mut out = vec![
        // CREATE or ALTER: re-running a bind must reset the password (that is
        // how a rotation reaches the server) without failing on the second
        // pass.
        //
        // `$apprafter$` rather than `$$`: a password containing `$$` would
        // otherwise terminate the block body early. Verified against a real
        // server with exactly such a password.
        format!(
            "DO $apprafter$ BEGIN IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = {}) \
             THEN CREATE ROLE {role_q} LOGIN PASSWORD {pw}; \
             ELSE ALTER ROLE {role_q} LOGIN PASSWORD {pw}; END IF; END $apprafter$;",
            quote_literal(consumer_role)
        ),
        format!("GRANT {} TO {role_q};", quote_ident(group)),
        format!(
            "GRANT CONNECT ON DATABASE {} TO {};",
            quote_ident(database),
            quote_ident(group)
        ),
    ];

    if access == Access::ReadWrite {
        // THE line. Without it every object this consumer creates is owned by
        // the consumer, and the next consumer cannot read it.
        out.push(format!(
            "ALTER ROLE {role_q} IN DATABASE {} SET ROLE {};",
            quote_ident(database),
            quote_ident(&owner)
        ));
    }
    out
}

/// Statements that give the reader group SELECT on everything in the shared
/// database, now and in future.
///
/// Run once per reconcile of the SharedDatabase, not per bind: the grants are
/// to the GROUP, so a consumer that joins later inherits them with nothing
/// further to do. The `ALTER DEFAULT PRIVILEGES FOR ROLE <owner>` line is what
/// makes that true for tables that do not exist yet — and it works precisely
/// because `bind_consumer`'s `SET ROLE` guarantees every future table is
/// created BY the owner group.
pub fn grant_reader(namespace: &str, shared_name: &str, database: &str) -> Vec<String> {
    let owner = shared_group(namespace, shared_name);
    let reader = shared_reader_group(namespace, shared_name);
    let (o, r, d) = (
        quote_ident(&owner),
        quote_ident(&reader),
        quote_ident(database),
    );
    vec![
        format!("GRANT CONNECT ON DATABASE {d} TO {r};"),
        format!("GRANT USAGE, CREATE ON SCHEMA public TO {o};"),
        format!("GRANT USAGE ON SCHEMA public TO {r};"),
        format!("GRANT SELECT ON ALL TABLES IN SCHEMA public TO {r};"),
        format!("GRANT SELECT ON ALL SEQUENCES IN SCHEMA public TO {r};"),
        format!(
            "ALTER DEFAULT PRIVILEGES FOR ROLE {o} IN SCHEMA public GRANT SELECT ON TABLES TO {r};"
        ),
        format!(
            "ALTER DEFAULT PRIVILEGES FOR ROLE {o} IN SCHEMA public GRANT SELECT ON SEQUENCES TO {r};"
        ),
    ]
}

/// Statements that revoke ONE consumer, for claim GC.
///
/// The shared database and its data are never touched — that is the property
/// the whole CRD exists to provide. Only this consumer's own role goes.
pub fn unbind_consumer(consumer_role: &str) -> Vec<String> {
    vec![
        // GRANT first, and it is not a formality — it is the same distinction
        // that corrected §3.1 of the ADR, arriving one function later.
        //
        // `apprafter_admin` CREATED this role, so it holds ADMIN OPTION on it.
        // `REASSIGN OWNED BY` does not accept admin option: it wants the
        // PRIVILEGES OF the role, i.e. membership. Measured on PostgreSQL
        // 18.4 by `e2e/shared-pg-sql-check.sh` step 7, which is where this
        // statement was executed for the first time:
        //
        //   ERROR:  permission denied to reassign objects
        //   DETAIL: Only roles with privileges of role "claim_apps_rep_pg"
        //           may reassign objects owned by it.
        //
        // Admin option is exactly the right to grant the role, so the platform
        // role can give itself membership; it vanishes with the role two
        // statements later.
        //
        // REASSIGN before DROP OWNED: a consumer should own nothing, because
        // `SET ROLE` put everything it created under the group — but a
        // consumer that connected before that ALTER landed would own tables,
        // and `DROP OWNED BY` alone would DELETE them. Reassigning first means
        // the worst case is an object that changes hands, not one that is
        // destroyed with the application that happened to create it.
        format!(
            "DO $apprafter$ BEGIN IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = {lit}) \
             THEN EXECUTE format('GRANT %I TO CURRENT_USER', {lit}); \
             EXECUTE format('REASSIGN OWNED BY %I TO CURRENT_USER', {lit}); \
             EXECUTE format('DROP OWNED BY %I', {lit}); \
             EXECUTE format('DROP ROLE %I', {lit}); END IF; END $apprafter$;",
            lit = quote_literal(consumer_role)
        ),
    ]
}

/// Statements that drop a shared database's two groups, for the delete of
/// the `SharedDatabase` itself at `refCount == 0`.
///
/// Run AFTER the database is gone. A role that owns a database cannot be
/// dropped — Postgres refuses with `role "…" cannot be dropped because some
/// objects depend on it` — and the owning group owns this one by design, so
/// the order is not a preference.
///
/// Guarded on existence, like every other builder here, because a controller
/// re-runs its cleanup: a second pass after a partial failure must complete
/// rather than fail on the half already done.
pub fn drop_groups(namespace: &str, name: &str) -> Vec<String> {
    let owner = shared_group(namespace, name);
    let reader = shared_reader_group(namespace, name);
    [reader, owner]
        .iter()
        .map(|role| {
            format!(
                "DO $apprafter$ BEGIN IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = {lit}) \
                 THEN EXECUTE format('DROP OWNED BY %I', {lit}); \
                 EXECUTE format('DROP ROLE %I', {lit}); END IF; END $apprafter$;",
                lit = quote_literal(role)
            )
        })
        .collect()
}

/// The query that answers whether the running operand image provides an
/// extension (ADR 0066 §4.2). `$1` is the extension name.
///
/// Asked on reconcile rather than trusted from a list, because whether
/// `vector` exists is a property of the IMAGE and the image is not pinned:
/// a CNPG bump can take it away under a live database, and the only honest
/// answer comes from the server.
pub const EXTENSION_AVAILABLE_QUERY: &str = "SELECT 1 FROM pg_available_extensions WHERE name = $1";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropping_the_groups_takes_the_reader_first() {
        // The owning group owns the database by design, and Postgres refuses
        // to drop a role that owns one. The reader owns nothing, so it can go
        // either way — but the OWNER must be last, after the database itself
        // is gone, and pinning the order here is what keeps a later "tidy the
        // list" edit from reversing it.
        let stmts = drop_groups("apps", "orders");
        assert_eq!(stmts.len(), 2);
        assert!(stmts[0].contains("'shd_apps_orders_ro'"), "{stmts:?}");
        assert!(stmts[1].contains("'shd_apps_orders'"), "{stmts:?}");
        // Matched on the QUOTED literal, not on a bare substring: the
        // statement body says `FROM pg_roles`, which contains `_ro` and made
        // the first version of this assertion fail against correct output.
        assert!(!stmts[1].contains("'shd_apps_orders_ro'"), "{stmts:?}");
    }

    #[test]
    fn dropping_a_group_that_is_already_gone_is_a_no_op() {
        // A controller re-runs its cleanup; a second pass after a partial
        // failure must complete rather than fail on the half already done.
        for stmt in drop_groups("apps", "orders") {
            assert!(stmt.contains("IF EXISTS"), "{stmt}");
        }
    }

    #[test]
    fn a_shared_group_cannot_collide_with_a_claim_role() {
        // Both fold through pg_identifier, so the prefix is the only thing
        // keeping a SharedDatabase called `x` apart from a claim called `x`.
        let group = shared_group("apps", "orders");
        assert_eq!(group, "shd_apps_orders");
        assert!(!group.starts_with("claim_"));
        assert_ne!(group, pg_identifier("apps", "orders"));
        assert_eq!(shared_reader_group("apps", "orders"), "shd_apps_orders_ro");
    }

    #[test]
    fn creating_the_groups_also_grants_the_platform_role_set_membership() {
        // Measured: `CREATE DATABASE ... OWNER <group>` fails with `must be
        // able to SET ROLE` unless the creator is a MEMBER, not merely the
        // group's admin. Dropping this grant is the kind of tidy-up that
        // passes review and fails at provision time.
        let stmts = create_groups("apps", "orders", "apprafter_admin");
        let all = stmts.join("\n");
        assert!(
            all.contains("CREATE ROLE \"shd_apps_orders\" NOLOGIN"),
            "{all}"
        );
        assert!(
            all.contains("CREATE ROLE \"shd_apps_orders_ro\" NOLOGIN"),
            "{all}"
        );
        assert!(
            all.contains("GRANT \"shd_apps_orders\" TO \"apprafter_admin\" WITH SET TRUE"),
            "{all}"
        );
        assert!(
            all.contains("GRANT \"shd_apps_orders_ro\" TO \"apprafter_admin\" WITH SET TRUE"),
            "{all}"
        );
    }

    #[test]
    fn every_create_is_guarded_so_a_reconcile_can_re_run_it() {
        // Postgres has no CREATE ROLE IF NOT EXISTS, and a controller runs
        // this on every reconcile.
        for s in create_groups("apps", "orders", "apprafter_admin") {
            if s.contains("CREATE ROLE") {
                assert!(
                    s.contains("IF NOT EXISTS (SELECT 1 FROM pg_roles"),
                    "unguarded CREATE: {s}"
                );
            }
        }
    }

    #[test]
    fn a_read_write_bind_sets_the_role_so_objects_belong_to_the_group() {
        // THE line of the whole design. Its absence is invisible until a
        // SECOND consumer tries to read the first's tables.
        let stmts = bind_consumer(
            "apps",
            "orders",
            "claim_apps_web_pg",
            "shd_apps_orders",
            "pw",
            Access::ReadWrite,
        );
        let all = stmts.join("\n");
        assert!(
            all.contains(
                "ALTER ROLE \"claim_apps_web_pg\" IN DATABASE \"shd_apps_orders\" \
                 SET ROLE \"shd_apps_orders\""
            ),
            "{all}"
        );
        assert!(
            all.contains("GRANT \"shd_apps_orders\" TO \"claim_apps_web_pg\""),
            "{all}"
        );
    }

    #[test]
    fn a_read_only_bind_joins_the_reader_group_and_never_sets_role() {
        // A ro consumer must NOT get SET ROLE: that would make it a member of
        // the owning group for object creation, which is the opposite of
        // read-only.
        let stmts = bind_consumer(
            "apps",
            "orders",
            "claim_apps_rep_pg",
            "shd_apps_orders",
            "pw",
            Access::ReadOnly,
        );
        let all = stmts.join("\n");
        assert!(
            all.contains("GRANT \"shd_apps_orders_ro\" TO \"claim_apps_rep_pg\""),
            "{all}"
        );
        assert!(
            !all.contains("SET ROLE"),
            "a read-only consumer must not SET ROLE to the owning group: {all}"
        );
    }

    #[test]
    fn a_password_is_quoted_as_a_literal_and_cannot_escape_the_statement() {
        // It cannot be a bind parameter: CREATE ROLE is a utility statement
        // and PostgreSQL refuses to prepare one (measured). So the defence is
        // literal quoting plus a dollar-quote tag the password cannot contain
        // accidentally.
        let all = bind_consumer(
            "apps",
            "orders",
            "claim_apps_web_pg",
            "shd_apps_orders",
            "p'w$$q",
            Access::ReadWrite,
        )
        .join("\n");
        assert!(all.contains("PASSWORD 'p''w$$q'"), "{all}");
        assert!(
            all.contains("$apprafter$") && !all.contains("DO $$"),
            "the block must use a tag a password cannot terminate: {all}"
        );
    }

    #[test]
    fn a_re_bind_resets_the_password_instead_of_failing() {
        // Re-running a bind is how a rotation reaches the server, and a
        // controller re-runs everything.
        let all = bind_consumer(
            "apps",
            "orders",
            "claim_apps_web_pg",
            "shd_apps_orders",
            "secret",
            Access::ReadWrite,
        )
        .join("\n");
        assert!(
            all.contains("ELSE ALTER ROLE \"claim_apps_web_pg\" LOGIN PASSWORD 'secret'"),
            "{all}"
        );
    }

    #[test]
    fn the_reader_grant_covers_tables_that_do_not_exist_yet() {
        // ALTER DEFAULT PRIVILEGES FOR ROLE <owner> is what makes a later
        // migration's table readable without re-running any grant — and it is
        // correct only because the rw bind's SET ROLE guarantees the owner
        // group creates them.
        let all = grant_reader("apps", "orders", "shd_apps_orders").join("\n");
        assert!(
            all.contains(
                "ALTER DEFAULT PRIVILEGES FOR ROLE \"shd_apps_orders\" IN SCHEMA public \
                 GRANT SELECT ON TABLES TO \"shd_apps_orders_ro\""
            ),
            "{all}"
        );
        assert!(
            all.contains("GRANT SELECT ON ALL TABLES IN SCHEMA public"),
            "{all}"
        );
        assert!(
            all.contains("GRANT USAGE, CREATE ON SCHEMA public TO \"shd_apps_orders\""),
            "the owner group must be able to create in public: {all}"
        );
    }

    #[test]
    fn unbinding_touches_only_the_consumer_role() {
        let all = unbind_consumer("claim_apps_web_pg").join("\n");
        assert!(all.contains("DROP ROLE"), "{all}");
        assert!(
            !all.contains("DROP DATABASE") && !all.contains("DROP SCHEMA"),
            "a consumer's GC must never reach the shared data: {all}"
        );
    }

    #[test]
    fn an_unrecognised_access_value_reads_as_read_only() {
        // The webhook constrains the field, so an unexpected value means
        // something upstream is wrong — and the safe reading of that is the
        // lesser privilege.
        assert_eq!(Access::from_spec(Some("rw")), Access::ReadWrite);
        assert_eq!(Access::from_spec(None), Access::ReadWrite);
        assert_eq!(Access::from_spec(Some("ro")), Access::ReadOnly);
        assert_eq!(Access::from_spec(Some("admin")), Access::ReadOnly);
        assert_eq!(Access::from_spec(Some("")), Access::ReadOnly);
    }

    /// Emit the exact statement sequence a shared database is built from, for
    /// `e2e/shared-pg-sql-check.sh` to execute against a real PostgreSQL.
    ///
    /// `#[ignore]`d: it asserts nothing on its own, it is a source of truth for
    /// the script. The alternative — writing the SQL out a second time in the
    /// script — would test a copy, which is the one thing worth avoiding here:
    /// the whole reason this check exists is that reasoning about these
    /// statements was already wrong once (`PASSWORD $1` cannot work).
    #[test]
    #[ignore]
    fn print_statements_for_the_live_sql_check() {
        let ns = "apps";
        let shared = "orders";
        let db = shared_group(ns, shared);
        println!("-- @@SETUP");
        for s in create_groups(ns, shared, "apprafter_admin") {
            println!("{s}");
        }
        println!(
            "CREATE DATABASE {} OWNER {};",
            quote_ident(&db),
            quote_ident(&shared_group(ns, shared))
        );
        println!("-- @@INDB");
        for s in grant_reader(ns, shared, &db) {
            println!("{s}");
        }
        for (role, access) in [
            ("claim_apps_web_pg", Access::ReadWrite),
            ("claim_apps_api_pg", Access::ReadWrite),
            ("claim_apps_rep_pg", Access::ReadOnly),
        ] {
            for s in bind_consumer(ns, shared, role, &db, "p'w$$q", access) {
                println!("{s}");
            }
        }
        // The UNBIND, which had never been executed anywhere. It was written,
        // unit-tested for the shape of its strings, given no caller, and then
        // given one — and the first live run failed on statement #0 with an
        // error the type could only render as "db error". Printing it here
        // puts it on the same live server as everything above, a minute's
        // cycle instead of a walk's.
        println!("-- @@UNBIND");
        for s in unbind_consumer("claim_apps_rep_pg") {
            println!("{s}");
        }
        // ...and the group drop, for the same reason: `drop_backing`'s doc
        // comment described it long before a builder existed.
        println!("-- @@DROPGROUPS");
        for s in drop_groups(ns, shared) {
            println!("{s}");
        }
        println!("-- @@END");
    }

    #[test]
    fn identifiers_are_quoted_and_a_quote_cannot_escape() {
        assert_eq!(quote_ident("shd_apps_orders"), "\"shd_apps_orders\"");
        assert_eq!(quote_ident("we\"ird"), "\"we\"\"ird\"");
    }
}
