-- migrate:up

-- Config type PricingRuleConfiguration (id 100, prefix
-- `prule`) of nanoservice `catalog`: the type's registration
-- row and its subtype table pair in the framework's temporal shape. The
-- live table INHERITS its history table; the versioning() trigger closes a
-- version on every change and writes history — never application code, and
-- an unchanged re-apply writes nothing. A reference field is a peer FK:
-- SET NULL, never CASCADE (a cascade would delete this subtype row alone and
-- orphan its base row), and nullable for that reason. Evolve columns with
-- `ALTER TABLE <t>_history ADD COLUMN …` (INHERITS propagates), never
-- `ALTER TABLE ONLY`.

INSERT INTO basable_config.configuration_object_type (id, name)
VALUES (100, 'PricingRuleConfiguration');

CREATE TABLE basable_config.pricing_rule_configuration_history (
    id            UUID NOT NULL,
    system_period TSTZRANGE NOT NULL DEFAULT tstzrange(current_timestamp, NULL),
    rate_cents BIGINT NOT NULL DEFAULT 0,
    currency TEXT NOT NULL DEFAULT ''
);
CREATE UNIQUE INDEX pricing_rule_configuration_history_lookup
    ON basable_config.pricing_rule_configuration_history (id, lower(system_period),
        coalesce(upper(system_period), 'infinity') DESC);

CREATE TABLE basable_config.pricing_rule_configuration (
    PRIMARY KEY (id),
    FOREIGN KEY (id) REFERENCES basable_config.configuration_object(id) ON DELETE CASCADE
) INHERITS (basable_config.pricing_rule_configuration_history);

CREATE TRIGGER versioning_trigger
    BEFORE INSERT OR UPDATE OR DELETE ON basable_config.pricing_rule_configuration
    FOR EACH ROW EXECUTE PROCEDURE basable_config.versioning('system_period', 'basable_config.pricing_rule_configuration_history', true, true);

-- migrate:down
DROP TABLE IF EXISTS basable_config.pricing_rule_configuration;
DROP TABLE IF EXISTS basable_config.pricing_rule_configuration_history;
DELETE FROM basable_config.configuration_object_type WHERE id = 100;
