//! Seed files and the items in them: the on-disk envelope, the natural key,
//! and the filename environment scope. The format is the platform's:
//! `{configSetName, items: [{"@type": "<package>.<Message>", header:
//! {namespace: "#{NamespaceConfiguration:<name>}", name, labels}, …fields}]}`
//! — each item is one config message in protobuf JSON plus the `@type`
//! control key. There is no operation key: an item in the files is stored,
//! and a loader-managed object absent from them is pruned.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::str::FromStr;

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::error::ConfigError;
use crate::reference::parse_ref;
use crate::types::{ConfigTypes, NAMESPACE_TYPE, TypeInfo};

const TYPE_KEY: &str = "@type";

/// Which deployment environment a load serves. The closed vocabulary shared
/// by the load parameter and filename scope tokens; a value outside it
/// fails loudly rather than silently scoping files in or out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Environment {
    /// Local development.
    Dev,
    /// Production.
    Prod,
    /// Automated tests.
    Test,
}

impl Environment {
    /// The token, as it appears in a filename.
    pub fn as_str(self) -> &'static str {
        match self {
            Environment::Dev => "dev",
            Environment::Prod => "prod",
            Environment::Test => "test",
        }
    }
}

impl fmt::Display for Environment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Environment {
    type Err = ConfigError;

    fn from_str(s: &str) -> Result<Environment, ConfigError> {
        match s {
            "dev" => Ok(Environment::Dev),
            "prod" => Ok(Environment::Prod),
            "test" => Ok(Environment::Test),
            other => Err(ConfigError::Environment(other.to_owned())),
        }
    }
}

/// Whether the seed file named `base` is in scope for `env`. Convention:
/// `<name>[.<env>…].json` — dot-separated tokens between the semantic name
/// and the extension scope the file to exactly those environments
/// (`pricing.prod.json`, `server-types.dev.test.json`); a bare
/// `pricing.json` applies everywhere. Dots are reserved for environment
/// tokens, so every token after the first segment must be a known
/// environment: under a lenient rule `pricing.pord.json` would be
/// indistinguishable from an unscoped file and silently apply everywhere.
pub fn file_applies_to(base: &str, env: Environment) -> Result<bool, ConfigError> {
    let stem = base.strip_suffix(".json").unwrap_or(base);
    let mut tokens = stem.split('.');
    tokens.next(); // the semantic name
    let mut scoped = false;
    let mut applies = false;
    for tok in tokens {
        let file_env: Environment = tok.parse().map_err(|_| ConfigError::Seed {
            file: base.to_owned(),
            reason: format!("unknown environment {tok:?} in filename (valid: dev, prod, test)"),
        })?;
        scoped = true;
        applies |= file_env == env;
    }
    Ok(!scoped || applies)
}

/// The natural key of a configuration object: its type name, the NAME of
/// its namespace (empty for a namespace), and its own name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ItemName {
    /// The type name.
    pub type_name: String,
    /// The namespace's name, empty for a namespace object.
    pub namespace: String,
    /// The object's name.
    pub name: String,
}

impl ItemName {
    /// A namespaced object's key.
    pub fn new(
        type_name: impl Into<String>,
        namespace: impl Into<String>,
        name: impl Into<String>,
    ) -> ItemName {
        ItemName {
            type_name: type_name.into(),
            namespace: namespace.into(),
            name: name.into(),
        }
    }

    /// A namespace's key.
    pub fn namespace(name: impl Into<String>) -> ItemName {
        ItemName::new(NAMESPACE_TYPE.name, "", name)
    }
}

impl fmt::Display for ItemName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.namespace.is_empty() {
            write!(f, "{}:{}", self.type_name, self.name)
        } else {
            write!(f, "{}:{}:{}", self.type_name, self.namespace, self.name)
        }
    }
}

/// One parsed configuration object from a seed file.
#[derive(Debug, Clone)]
pub struct Item {
    /// The natural key.
    pub name: ItemName,
    /// The type.
    pub type_info: TypeInfo,
    /// The item with `@type` removed: the message in protobuf JSON,
    /// references still unresolved.
    pub body: Value,
    /// The file the item came from (its base name), for diagnostics.
    pub source_file: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConfigSet {
    #[serde(default)]
    #[allow(dead_code)]
    config_set_name: String,
    #[serde(default)]
    items: Vec<Value>,
}

/// The bare message name of an `@type` value: the last segment after `/`
/// and `.` (`type.googleapis.com/orders.v1.PricingRuleConfiguration` and
/// `orders.v1.PricingRuleConfiguration` both name
/// `PricingRuleConfiguration`).
fn type_name_from_url(url: &str) -> &str {
    let after_slash = url.rsplit('/').next().unwrap_or(url);
    after_slash.rsplit('.').next().unwrap_or(after_slash)
}

/// Parses one item: the `@type` control key names the type, `header.name`
/// is the object's name, and `header.namespace` is a
/// `#{NamespaceConfiguration:<name>}` reference — a bare name is rejected so
/// the file says what it means. A namespace item must omit it; every other
/// item must carry it.
pub fn parse_item(
    types: &ConfigTypes,
    raw: &Value,
    source_file: &str,
) -> Result<Item, ConfigError> {
    let fail = |reason: String| ConfigError::Seed {
        file: source_file.to_owned(),
        reason,
    };
    let Value::Object(fields) = raw else {
        return Err(fail("item is not a JSON object".into()));
    };
    let type_url = fields
        .get(TYPE_KEY)
        .ok_or_else(|| fail(format!("item missing {TYPE_KEY:?}")))?
        .as_str()
        .ok_or_else(|| fail(format!("{TYPE_KEY:?} must be a string")))?;
    let type_name = type_name_from_url(type_url);
    let type_info = types.type_by_name(type_name).ok_or_else(|| {
        fail(format!(
            "unknown config type {type_name:?} (from {type_url:?})"
        ))
    })?;

    let header = fields.get("header").and_then(Value::as_object);
    let name = header
        .and_then(|h| h.get("name"))
        .and_then(Value::as_str)
        .filter(|n| !n.is_empty())
        .ok_or_else(|| fail("item missing header.name".into()))?
        .to_owned();
    let namespace = match header
        .and_then(|h| h.get("namespace"))
        .and_then(Value::as_str)
    {
        None | Some("") => String::new(),
        Some(raw) => {
            let inner = raw
                .strip_prefix("#{")
                .and_then(|r| r.strip_suffix('}'))
                .filter(|inner| !inner.contains('}'))
                .ok_or_else(|| {
                    fail(format!(
                        "header.namespace {raw:?} must be a #{{NamespaceConfiguration:<name>}} reference, not a bare name"
                    ))
                })?;
            let r = parse_ref(inner).map_err(|e| fail(format!("header.namespace: {e}")))?;
            if r.type_name != NAMESPACE_TYPE.name || !r.namespace.is_empty() {
                return Err(fail(format!(
                    "header.namespace {raw:?} must reference a NamespaceConfiguration (#{{NamespaceConfiguration:<name>}})"
                )));
            }
            r.name
        }
    };
    let is_namespace = type_info.id == NAMESPACE_TYPE.id;
    if is_namespace && !namespace.is_empty() {
        return Err(fail(format!(
            "{name}: a namespace object must not declare a header.namespace"
        )));
    }
    if !is_namespace && namespace.is_empty() {
        return Err(fail(format!(
            "{type_name}:{name}: missing header.namespace"
        )));
    }

    let mut body: Map<String, Value> = fields.clone();
    body.remove(TYPE_KEY);
    Ok(Item {
        name: ItemName::new(type_info.name, namespace, name),
        type_info,
        body: Value::Object(body),
        source_file: source_file.to_owned(),
    })
}

/// Parses one seed file's text: `{configSetName, items: […]}`.
pub fn parse_seed(
    types: &ConfigTypes,
    text: &str,
    source_file: &str,
) -> Result<Vec<Item>, ConfigError> {
    let set: ConfigSet = serde_json::from_str(text).map_err(|e| ConfigError::Seed {
        file: source_file.to_owned(),
        reason: format!("parse: {e}"),
    })?;
    set.items
        .iter()
        .enumerate()
        .map(|(i, raw)| {
            parse_item(types, raw, source_file).map_err(|e| match e {
                ConfigError::Seed { file, reason } => ConfigError::Seed {
                    file,
                    reason: format!("item {i}: {reason}"),
                },
                other => other,
            })
        })
        .collect()
}

/// Reads every `*.json` under `dir` whose filename scope includes `env`,
/// keyed by natural name, rejecting duplicates. Files scoped to other
/// environments are skipped BEFORE parsing, so the same natural key may
/// appear in two files with disjoint scopes (per-environment variants of
/// one object). A directory without any seed file is a wrong path or a
/// broken image; a directory whose every file is scoped away is legitimate.
pub(crate) fn read_items(
    types: &ConfigTypes,
    dir: &Path,
    env: Environment,
) -> Result<BTreeMap<ItemName, Item>, ConfigError> {
    let list = |reason: String| ConfigError::Seed {
        file: dir.display().to_string(),
        reason,
    };
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| list(format!("list config files: {e}")))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    if files.is_empty() {
        return Err(list("no config files (*.json) found".into()));
    }
    let mut items: BTreeMap<ItemName, Item> = BTreeMap::new();
    for file in files {
        let base = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !file_applies_to(&base, env)? {
            continue;
        }
        let text = std::fs::read_to_string(&file).map_err(|e| ConfigError::Seed {
            file: base.clone(),
            reason: format!("read: {e}"),
        })?;
        for item in parse_seed(types, &text, &base)? {
            if let Some(prev) = items.get(&item.name) {
                return Err(ConfigError::Seed {
                    file: base.clone(),
                    reason: format!(
                        "duplicate config object {} (also in {})",
                        item.name, prev.source_file
                    ),
                });
            }
            items.insert(item.name.clone(), item);
        }
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ConfigTypesBuilder;

    #[test]
    fn filenames_scope_by_environment() {
        assert!(file_applies_to("pricing.json", Environment::Prod).unwrap());
        assert!(file_applies_to("pricing.prod.json", Environment::Prod).unwrap());
        assert!(!file_applies_to("pricing.prod.json", Environment::Dev).unwrap());
        assert!(file_applies_to("types.dev.test.json", Environment::Test).unwrap());
        assert!(!file_applies_to("types.dev.test.json", Environment::Prod).unwrap());
        let err = file_applies_to("pricing.pord.json", Environment::Prod).unwrap_err();
        assert!(err.to_string().contains("\"pord\""), "{err}");
        assert_eq!("prod".parse::<Environment>().unwrap(), Environment::Prod);
        assert!("staging".parse::<Environment>().is_err());
    }

    fn types() -> ConfigTypes {
        ConfigTypesBuilder::new().build().unwrap()
    }

    #[test]
    fn type_urls_reduce_to_the_message_name() {
        assert_eq!(
            type_name_from_url("type.googleapis.com/orders.v1.PricingRuleConfiguration"),
            "PricingRuleConfiguration"
        );
        assert_eq!(
            type_name_from_url("config.v1.NamespaceConfiguration"),
            "NamespaceConfiguration"
        );
        assert_eq!(type_name_from_url("Bare"), "Bare");
    }

    #[test]
    fn a_seed_file_parses_into_items() {
        let text = r##"{
            "configSetName": "base-namespaces",
            "items": [
                {"@type": "config.v1.NamespaceConfiguration",
                 "header": {"name": "billing", "labels": {"tier": "a"}},
                 "display_name": "Billing"}
            ]
        }"##;
        let items = parse_seed(&types(), text, "namespaces.json").unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, ItemName::namespace("billing"));
        assert_eq!(items[0].type_info, NAMESPACE_TYPE);
        assert!(
            items[0].body.get("@type").is_none(),
            "the control key is stripped"
        );
        assert_eq!(items[0].body["display_name"], "Billing");
    }

    #[test]
    fn malformed_items_are_named() {
        let cases = [
            (r##"{"header":{"name":"x"}}"##, "missing \"@type\""),
            (
                r##"{"@type":"config.v1.Widget","header":{"name":"x"}}"##,
                "unknown config type",
            ),
            (
                r##"{"@type":"config.v1.NamespaceConfiguration"}"##,
                "missing header.name",
            ),
            (
                r##"{"@type":"config.v1.NamespaceConfiguration","header":{"name":"x","namespace":"#{NamespaceConfiguration:y}"}}"##,
                "must not declare",
            ),
            (
                r##"{"@type":"config.v1.NamespaceConfiguration","header":{"name":"x","namespace":"bare"}}"##,
                "not a bare name",
            ),
            (r##"[1]"##, "not a JSON object"),
        ];
        for (text, want) in cases {
            let raw: Value = serde_json::from_str(text).unwrap();
            let err = parse_item(&types(), &raw, "f.json").unwrap_err();
            assert!(err.to_string().contains(want), "{err} should name {want:?}");
        }
    }
}
