//! The reserved conformance type: its nanoservice, migrations, spec and
//! status, and the adapter mapping them onto the typed tables. The adapter
//! is a dumb column mapper (the payoff of invariant 3): every status write
//! reaches the database under exact claim authority, so there are no
//! provenance flags, no monotonic merges, no conditional column guards.

use std::path::Path;

use basable_db::migrate::{self, MigrateError, Migration};
use basable_db::{MigratorPool, Nanoservice, Stateful};
use basable_processingobject::{Adapter, Object, ProcessingObjectType, Ref, Row, Tx};
use sqlx::Row as _;
use uuid::Uuid;

/// The conformance nanoservice.
pub struct Conformance;

impl Nanoservice for Conformance {
    const NAME: &'static str = "conformance";
}

impl Stateful for Conformance {}

/// The type's registry name.
pub const TYPE_NAME: &str = "conformance";
/// The type's registry key: a high reserved value no production type uses.
pub const TYPE_KEY: i16 = 32000;
/// The type's public-id prefix (`external_id` is `ek_…`).
pub const PUBLIC_ID_PREFIX: &str = "ek";

/// The conformance type's desired state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Spec {
    /// How many widgets to provision.
    pub widgets: i32,
    /// Arbitrary content the simulator stores.
    pub content: String,
}

/// The conformance type's observed state. No activity marker, no
/// provenance: status is written only under exact claim authority.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    /// How many widgets the provider holds.
    pub provisioned_widgets: i32,
    /// The provider's id for the resource.
    pub external_id: String,
}

/// The adapter over `conformance_spec`, `conformance_status` and the
/// archive.
#[derive(Debug, Clone, Copy, Default)]
pub struct ConformanceAdapter;

impl Adapter<Spec, Status> for ConformanceAdapter {
    async fn insert_spec(&self, tx: &mut Tx<'_>, r: &Ref, spec: &Spec) -> Result<(), sqlx::Error> {
        sqlx::query("INSERT INTO conformance_spec (id, widgets, content) VALUES ($1, $2, $3)")
            .bind(r.id)
            .bind(spec.widgets)
            .bind(&spec.content)
            .execute(tx)
            .await
            .map(|_| ())
    }

    async fn insert_status(
        &self,
        tx: &mut Tx<'_>,
        r: &Ref,
        status: &Status,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO conformance_status (id, provisioned_widgets, external_id) VALUES ($1, $2, $3)",
        )
        .bind(r.id)
        .bind(status.provisioned_widgets)
        .bind(&status.external_id)
        .execute(tx)
        .await
        .map(|_| ())
    }

    async fn read_rows(
        &self,
        tx: &mut Tx<'_>,
        ids: &[Uuid],
    ) -> Result<Vec<Row<Spec, Status>>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT s.id, s.widgets, s.content, t.provisioned_widgets, t.external_id
             FROM conformance_spec s
             JOIN conformance_status t USING (id)
             WHERE s.id = ANY($1)",
        )
        .bind(ids)
        .fetch_all(tx)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(Row {
                    id: row.try_get("id")?,
                    spec: Spec {
                        widgets: row.try_get("widgets")?,
                        content: row.try_get("content")?,
                    },
                    status: Status {
                        provisioned_widgets: row.try_get("provisioned_widgets")?,
                        external_id: row.try_get("external_id")?,
                    },
                })
            })
            .collect()
    }

    async fn write_spec(&self, tx: &mut Tx<'_>, r: &Ref, spec: &Spec) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE conformance_spec SET widgets = $2, content = $3, updated_at = clock_timestamp()
             WHERE id = $1",
        )
        .bind(r.id)
        .bind(spec.widgets)
        .bind(&spec.content)
        .execute(tx)
        .await
        .map(|_| ())
    }

    /// Every column written unconditionally. The status CHECK
    /// (`provisioned_widgets >= 0`) lets a test drive the savepoint-rollback
    /// to Retry path by returning a negative value.
    async fn write_status(
        &self,
        tx: &mut Tx<'_>,
        r: &Ref,
        status: &Status,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE conformance_status
             SET provisioned_widgets = $2, external_id = $3, updated_at = clock_timestamp()
             WHERE id = $1",
        )
        .bind(r.id)
        .bind(status.provisioned_widgets)
        .bind(&status.external_id)
        .execute(tx)
        .await
        .map(|_| ())
    }

    /// A durable archive row that outlives the deleted envelope, written
    /// atomically with the envelope DELETE. `ON CONFLICT DO NOTHING` makes
    /// it idempotent under an ambiguous completion commit (invariant 5).
    async fn finalize_delete(
        &self,
        tx: &mut Tx<'_>,
        obj: &Object<Spec, Status>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO conformance_archive
                 (id, widgets, content, provisioned_widgets, external_id, archived_at)
             VALUES ($1, $2, $3, $4, $5, clock_timestamp())
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(obj.id)
        .bind(obj.spec.widgets)
        .bind(&obj.spec.content)
        .bind(obj.status.provisioned_widgets)
        .bind(&obj.status.external_id)
        .execute(tx)
        .await
        .map(|_| ())
    }
}

/// The fully populated declaration of the conformance type.
pub fn conformance_type() -> ProcessingObjectType<Spec, Status, ConformanceAdapter> {
    ProcessingObjectType::new(TYPE_NAME, TYPE_KEY, PUBLIC_ID_PREFIX, ConformanceAdapter)
}

/// The deterministic envelope name for an object id, so a create replay
/// presents the identity the object was born with.
pub fn identity_name(id: Uuid) -> String {
    format!("conformance-{id}")
}

const INIT_SQL: &str = include_str!("../migrations/20260101000300_conformance_init.sql");
const TYPE_SQL: &str = include_str!("../migrations/20260101000301_conformance_type.sql");

/// The conformance nanoservice's migrations, in order.
pub fn migrations() -> Result<Vec<Migration>, MigrateError> {
    Ok(vec![
        migrate::parse(Path::new("20260101000300_conformance_init.sql"), INIT_SQL)?,
        migrate::parse(Path::new("20260101000301_conformance_type.sql"), TYPE_SQL)?,
    ])
}

/// Installs the conformance nanoservice in an already-migrated tenant
/// database (the framework migration must have run): its role and schema,
/// the registry row, the partition, and the typed tables. Idempotent
/// through the migration ledger, so a shared database installs it once.
pub async fn apply_schema(migrator: &MigratorPool) -> Result<(), MigrateError> {
    let files = migrations()?;
    migrate::apply(migrator, &files, &[]).await.map(|_| ())
}
