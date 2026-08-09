//! Compile-time embedded PostgreSQL migrations without enabling SQLx's query
//! macro feature set. Keeping the migration bytes in the release artifact avoids
//! a runtime filesystem dependency and prevents unused database-driver crates
//! from entering the locked production dependency graph.

use std::borrow::Cow;

use sqlx::migrate::{Migration, MigrationType, Migrator};

pub(crate) fn migrator() -> Migrator {
    let migrations = vec![
        up(
            1,
            "control plane",
            include_str!("../../../infra/postgres/migrations/0001_control_plane.up.sql"),
        ),
        down(
            1,
            "control plane",
            include_str!("../../../infra/postgres/migrations/0001_control_plane.down.sql"),
        ),
        up(
            2,
            "project service settings",
            include_str!("../../../infra/postgres/migrations/0002_project_service_settings.up.sql"),
        ),
        down(
            2,
            "project service settings",
            include_str!(
                "../../../infra/postgres/migrations/0002_project_service_settings.down.sql"
            ),
        ),
        up(
            3,
            "email templates outbox",
            include_str!("../../../infra/postgres/migrations/0003_email_templates_outbox.up.sql"),
        ),
        down(
            3,
            "email templates outbox",
            include_str!("../../../infra/postgres/migrations/0003_email_templates_outbox.down.sql"),
        ),
        up(
            4,
            "security workflows",
            include_str!("../../../infra/postgres/migrations/0004_security_workflows.up.sql"),
        ),
        down(
            4,
            "security workflows",
            include_str!("../../../infra/postgres/migrations/0004_security_workflows.down.sql"),
        ),
        up(
            5,
            "bounded security state",
            include_str!("../../../infra/postgres/migrations/0005_bounded_security_state.up.sql"),
        ),
        down(
            5,
            "bounded security state",
            include_str!("../../../infra/postgres/migrations/0005_bounded_security_state.down.sql"),
        ),
        up(
            6,
            "platform billing",
            include_str!("../../../infra/postgres/migrations/0006_platform_billing.up.sql"),
        ),
        down(
            6,
            "platform billing",
            include_str!("../../../infra/postgres/migrations/0006_platform_billing.down.sql"),
        ),
        up(
            7,
            "usage billing",
            include_str!("../../../infra/postgres/migrations/0007_usage_billing.up.sql"),
        ),
        down(
            7,
            "usage billing",
            include_str!("../../../infra/postgres/migrations/0007_usage_billing.down.sql"),
        ),
        up(
            8,
            "project commerce",
            include_str!("../../../infra/postgres/migrations/0008_project_commerce.up.sql"),
        ),
        down(
            8,
            "project commerce",
            include_str!("../../../infra/postgres/migrations/0008_project_commerce.down.sql"),
        ),
        up(
            9,
            "instance onboarding",
            include_str!("../../../infra/postgres/migrations/0009_instance_onboarding.up.sql"),
        ),
        down(
            9,
            "instance onboarding",
            include_str!("../../../infra/postgres/migrations/0009_instance_onboarding.down.sql"),
        ),
        up(
            10,
            "instance billing catalog",
            include_str!("../../../infra/postgres/migrations/0010_instance_billing_catalog.up.sql"),
        ),
        down(
            10,
            "instance billing catalog",
            include_str!(
                "../../../infra/postgres/migrations/0010_instance_billing_catalog.down.sql"
            ),
        ),
        up(
            11,
            "decimal storage catalog",
            include_str!("../../../infra/postgres/migrations/0011_decimal_storage_catalog.up.sql"),
        ),
        down(
            11,
            "decimal storage catalog",
            include_str!(
                "../../../infra/postgres/migrations/0011_decimal_storage_catalog.down.sql"
            ),
        ),
        up(
            12,
            "legacy instance owner",
            include_str!("../../../infra/postgres/migrations/0012_legacy_instance_owner.up.sql"),
        ),
        down(
            12,
            "legacy instance owner",
            include_str!("../../../infra/postgres/migrations/0012_legacy_instance_owner.down.sql"),
        ),
        up(
            13,
            "instance setup readiness",
            include_str!("../../../infra/postgres/migrations/0013_instance_setup_readiness.up.sql"),
        ),
        down(
            13,
            "instance setup readiness",
            include_str!(
                "../../../infra/postgres/migrations/0013_instance_setup_readiness.down.sql"
            ),
        ),
        up(
            14,
            "observability",
            include_str!("../../../infra/postgres/migrations/0014_observability.up.sql"),
        ),
        down(
            14,
            "observability",
            include_str!("../../../infra/postgres/migrations/0014_observability.down.sql"),
        ),
        up(
            15,
            "control plane performance",
            include_str!(
                "../../../infra/postgres/migrations/0015_control_plane_performance.up.sql"
            ),
        ),
        down(
            15,
            "control plane performance",
            include_str!(
                "../../../infra/postgres/migrations/0015_control_plane_performance.down.sql"
            ),
        ),
        up(
            16,
            "authentication rate-limit dimensions",
            include_str!(
                "../../../infra/postgres/migrations/0016_auth_rate_limit_dimensions.up.sql"
            ),
        ),
        down(
            16,
            "authentication rate-limit dimensions",
            include_str!(
                "../../../infra/postgres/migrations/0016_auth_rate_limit_dimensions.down.sql"
            ),
        ),
    ];
    Migrator {
        migrations: Cow::Owned(migrations),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    }
}

fn up(version: i64, description: &'static str, sql: &'static str) -> Migration {
    Migration::new(
        version,
        Cow::Borrowed(description),
        MigrationType::ReversibleUp,
        Cow::Borrowed(sql),
        false,
    )
}

fn down(version: i64, description: &'static str, sql: &'static str) -> Migration {
    Migration::new(
        version,
        Cow::Borrowed(description),
        MigrationType::ReversibleDown,
        Cow::Borrowed(sql),
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embeds_ordered_reversible_migrations() {
        let migrator = migrator();
        let rows = migrator
            .iter()
            .map(|migration| (migration.version, migration.migration_type))
            .collect::<Vec<_>>();
        assert_eq!(
            rows,
            vec![
                (1, MigrationType::ReversibleUp),
                (1, MigrationType::ReversibleDown),
                (2, MigrationType::ReversibleUp),
                (2, MigrationType::ReversibleDown),
                (3, MigrationType::ReversibleUp),
                (3, MigrationType::ReversibleDown),
                (4, MigrationType::ReversibleUp),
                (4, MigrationType::ReversibleDown),
                (5, MigrationType::ReversibleUp),
                (5, MigrationType::ReversibleDown),
                (6, MigrationType::ReversibleUp),
                (6, MigrationType::ReversibleDown),
                (7, MigrationType::ReversibleUp),
                (7, MigrationType::ReversibleDown),
                (8, MigrationType::ReversibleUp),
                (8, MigrationType::ReversibleDown),
                (9, MigrationType::ReversibleUp),
                (9, MigrationType::ReversibleDown),
                (10, MigrationType::ReversibleUp),
                (10, MigrationType::ReversibleDown),
                (11, MigrationType::ReversibleUp),
                (11, MigrationType::ReversibleDown),
                (12, MigrationType::ReversibleUp),
                (12, MigrationType::ReversibleDown),
                (13, MigrationType::ReversibleUp),
                (13, MigrationType::ReversibleDown),
                (14, MigrationType::ReversibleUp),
                (14, MigrationType::ReversibleDown),
                (15, MigrationType::ReversibleUp),
                (15, MigrationType::ReversibleDown),
                (16, MigrationType::ReversibleUp),
                (16, MigrationType::ReversibleDown),
            ]
        );
        assert!(migrator.iter().all(|migration| !migration.sql.is_empty()));
    }
}
