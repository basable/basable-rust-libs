-- migrate:up

-- Config type PricingRuleConfiguration (id 100, prefix `prule`) of nanoservice
-- `orders`: the scaffolder's config-type migration, rendered by hand for this
-- fixture. The type row registers the id; the subtype table pair holds the
-- fields in the framework's temporal shape (history written only by the
-- trigger). A reference field (`supersedes`, a #{…} reference in the seed) is
-- a peer FK: SET NULL, never CASCADE, and nullable for that reason.

INSERT INTO basable_config.configuration_object_type (id, name) VALUES (100, 'PricingRuleConfiguration');

CREATE TABLE basable_config.pricing_rule_configuration_history (
    id            UUID NOT NULL,
    system_period TSTZRANGE NOT NULL DEFAULT tstzrange(current_timestamp, NULL),
    rate_cents    BIGINT NOT NULL DEFAULT 0,
    currency      TEXT NOT NULL DEFAULT '',
    supersedes    UUID
);
CREATE UNIQUE INDEX pricing_rule_configuration_history_lookup
    ON basable_config.pricing_rule_configuration_history (id, lower(system_period),
        coalesce(upper(system_period), 'infinity') DESC);

CREATE TABLE basable_config.pricing_rule_configuration (
    PRIMARY KEY (id),
    FOREIGN KEY (id) REFERENCES basable_config.configuration_object(id) ON DELETE CASCADE,
    FOREIGN KEY (supersedes) REFERENCES basable_config.configuration_object(id) ON DELETE SET NULL
) INHERITS (basable_config.pricing_rule_configuration_history);

CREATE TRIGGER versioning_trigger
    BEFORE INSERT OR UPDATE OR DELETE ON basable_config.pricing_rule_configuration
    FOR EACH ROW EXECUTE PROCEDURE basable_config.versioning('system_period', 'basable_config.pricing_rule_configuration_history', true, true);

-- migrate:down
DROP TABLE IF EXISTS basable_config.pricing_rule_configuration;
DROP TABLE IF EXISTS basable_config.pricing_rule_configuration_history;
DELETE FROM basable_config.configuration_object_type WHERE id = 100;
