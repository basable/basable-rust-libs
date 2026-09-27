-- migrate:up

-- Config type `pricing_rule` (id 100, prefix `prule`) of nanoservice
-- `orders`: the scaffolder's config-type migration, rendered by hand for
-- this fixture. The row is the type's registration; the objects live in
-- `basable_config.configuration_object` under it, written by the app login
-- (the loader at boot, the repository at runtime) and read by every
-- nanoservice role.

INSERT INTO basable_config.configuration_type (id, name, prefix)
VALUES (100, 'pricing_rule', 'prule')
ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, prefix = EXCLUDED.prefix;

-- migrate:down
DELETE FROM basable_config.configuration_object WHERE type_id = 100;
DELETE FROM basable_config.configuration_type WHERE id = 100;
