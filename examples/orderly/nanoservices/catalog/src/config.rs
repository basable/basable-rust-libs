//! The declarative config types this nanoservice owns: a person edits
//! `config/base/<name>.json`, the loader applies it at boot, and the
//! binders here map each message onto its subtype table in
//! `basable_config`. The messages are the proto types in
//! `proto/catalog/v1/config.proto`; a reference field arrives as the
//! referenced object's id.

use basable_config::{BinderError, ConfigHeader, ConfigTypesBuilder, TypeInfo, TypedBinder};
use sqlx::PgConnection;
use uuid::Uuid;

pub use proto::proto::catalog::v1::PricingRuleConfiguration;

/// `PricingRuleConfiguration` (`prule_…`), id 100.
pub const PRICING_RULE_TYPE: TypeInfo = TypeInfo {
    id: 100,
    name: "PricingRuleConfiguration",
    prefix: "prule",
};

/// The binder over `basable_config.pricing_rule_configuration`.
pub struct PricingRuleBinder;

impl TypedBinder for PricingRuleBinder {
    type Msg = PricingRuleConfiguration;

    fn type_info(&self) -> TypeInfo {
        PRICING_RULE_TYPE
    }

    fn header(&self, msg: &PricingRuleConfiguration) -> ConfigHeader {
        // The proto's header is a message field, absent when unset.
        let h = msg.header.as_option();
        ConfigHeader {
            namespace: h.map(|h| h.namespace.clone()).unwrap_or_default(),
            name: h.map(|h| h.name.clone()).unwrap_or_default(),
            external_id: h.map(|h| h.external_id.clone()).unwrap_or_default(),
            labels: h
                .map(|h| h.labels.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default(),
        }
    }

    async fn upsert(
        &self,
        tx: &mut PgConnection,
        id: Uuid,
        msg: &PricingRuleConfiguration,
    ) -> Result<(), BinderError> {
        sqlx::query(
            "INSERT INTO basable_config.pricing_rule_configuration (id, rate_cents, currency)
             VALUES ($1, $2, $3)
             ON CONFLICT (id) DO UPDATE SET
                 rate_cents = EXCLUDED.rate_cents,
                 currency = EXCLUDED.currency",
        )
        .bind(id)
        .bind(msg.rate_cents)
        .bind(&msg.currency)
        .execute(tx)
        .await?;
        Ok(())
    }

    async fn delete(&self, tx: &mut PgConnection, id: Uuid) -> Result<(), BinderError> {
        // Refuse with BinderError::StillReferenced here while another
        // object still names this one; a prune retries once it is gone.
        sqlx::query("DELETE FROM basable_config.pricing_rule_configuration WHERE id = $1")
            .bind(id)
            .execute(tx)
            .await?;
        Ok(())
    }

    async fn read(
        &self,
        conn: &mut PgConnection,
        id: Uuid,
    ) -> Result<Option<PricingRuleConfiguration>, BinderError> {
        let row: Option<(i64, String)> = sqlx::query_as(
            "SELECT rate_cents, currency
             FROM basable_config.pricing_rule_configuration WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(conn)
        .await?;
        Ok(row.map(|(rate_cents, currency)| PricingRuleConfiguration {
            rate_cents,
            currency,
            ..Default::default()
        }))
    }
}

/// Registers this nanoservice's config types with the app's builder.
pub fn register(b: &mut ConfigTypesBuilder) {
    b.register(PricingRuleBinder);
}
