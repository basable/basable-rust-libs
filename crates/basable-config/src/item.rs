//! Seed files and the items in them: the on-disk envelope, the natural key,
//! and the filename environment scope.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::str::FromStr;

use basable_core::labels::Labels;
use serde::Deserialize;
use serde_json::Value;

use crate::error::ConfigError;
use crate::types::{ConfigTypes, NAMESPACE_TYPE, TypeInfo};

/// The `apiVersion` every seed file carries.
pub const API_VERSION: &str = "basable.com/v1";

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

/// The per-item declarative operation. `store` (the default) upserts;
/// `delete` removes the object. There is no patch: declarative files restate
/// the whole object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    /// Create or update.
    Store,
    /// Remove.
    Delete,
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
    /// Store or delete.
    pub operation: Operation,
    /// `metadata.labels`.
    pub labels: Labels,
    /// The spec, references still unresolved.
    pub spec: Value,
    /// The file the item came from (its base name), for diagnostics.
    pub source_file: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SeedFile {
    api_version: String,
    kind: String,
    #[serde(default)]
    items: Vec<SeedItem>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SeedItem {
    metadata: Metadata,
    #[serde(default = "empty_object")]
    spec: Value,
    #[serde(default)]
    operation: Option<String>,
}

fn empty_object() -> Value {
    Value::Object(Default::default())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    #[serde(default)]
    namespace: String,
    name: String,
    #[serde(default)]
    labels: Labels,
}

/// Parses one seed file's text: `{apiVersion, kind, items: [{metadata:
/// {namespace, name, labels}, spec, operation}]}`. `kind` names the type;
/// a namespace item carries no `metadata.namespace`, every other item
/// must.
pub fn parse_seed(
    types: &ConfigTypes,
    text: &str,
    source_file: &str,
) -> Result<Vec<Item>, ConfigError> {
    let fail = |reason: String| ConfigError::Seed {
        file: source_file.to_owned(),
        reason,
    };
    let file: SeedFile = serde_json::from_str(text).map_err(|e| fail(format!("parse: {e}")))?;
    if file.api_version != API_VERSION {
        return Err(fail(format!(
            "apiVersion must be {API_VERSION:?}, got {:?}",
            file.api_version
        )));
    }
    let type_info = types
        .type_by_kind(&file.kind)
        .ok_or_else(|| fail(format!("unknown config type (kind) {:?}", file.kind)))?;
    let mut items = Vec::with_capacity(file.items.len());
    for (i, raw) in file.items.into_iter().enumerate() {
        let at = |reason: String| fail(format!("item {i}: {reason}"));
        if raw.metadata.name.is_empty() {
            return Err(at("metadata.name is required".into()));
        }
        let is_namespace = type_info.id == NAMESPACE_TYPE.id;
        if is_namespace && !raw.metadata.namespace.is_empty() {
            return Err(at("a namespace must not declare metadata.namespace".into()));
        }
        if !is_namespace && raw.metadata.namespace.is_empty() {
            return Err(at("metadata.namespace is required".into()));
        }
        let operation = match raw.operation.as_deref() {
            None | Some("store") => Operation::Store,
            Some("delete") => Operation::Delete,
            Some(other) => return Err(at(format!("unknown operation {other:?}"))),
        };
        if !raw.spec.is_object() {
            return Err(at("spec must be a JSON object".into()));
        }
        items.push(Item {
            name: ItemName::new(type_info.name, raw.metadata.namespace, raw.metadata.name),
            type_info,
            operation,
            labels: raw.metadata.labels,
            spec: raw.spec,
            source_file: source_file.to_owned(),
        });
    }
    Ok(items)
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
        assert!(err.to_string().contains("pord"), "{err}");
        assert_eq!("prod".parse::<Environment>().unwrap(), Environment::Prod);
        assert!("staging".parse::<Environment>().is_err());
    }

    #[derive(serde::Serialize, serde::Deserialize)]
    struct Rule {
        rate: i64,
    }

    fn types() -> ConfigTypes {
        let mut b = ConfigTypesBuilder::new();
        b.register::<Rule>(TypeInfo {
            id: 100,
            name: "pricing_rule",
            prefix: "prule",
        });
        b.build().unwrap()
    }

    #[test]
    fn a_seed_file_parses_into_items() {
        let text = r#"{
            "apiVersion": "basable.com/v1",
            "kind": "PricingRule",
            "items": [
                {"metadata": {"namespace": "default", "name": "standard", "labels": {"tier": "a"}},
                 "spec": {"rate": 3}},
                {"metadata": {"namespace": "default", "name": "old"}, "operation": "delete"}
            ]
        }"#;
        let items = parse_seed(&types(), text, "pricing_rule.json").unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].name.to_string(), "pricing_rule:default:standard");
        assert_eq!(items[0].operation, Operation::Store);
        assert_eq!(items[0].labels.get("tier").map(String::as_str), Some("a"));
        assert_eq!(items[0].spec, serde_json::json!({"rate": 3}));
        assert_eq!(items[1].operation, Operation::Delete);
        assert_eq!(items[1].spec, serde_json::json!({}));

        let ns = parse_seed(
            &types(),
            r#"{"apiVersion":"basable.com/v1","kind":"Namespace","items":[{"metadata":{"name":"default"}}]}"#,
            "namespace.json",
        )
        .unwrap();
        assert_eq!(ns[0].name, ItemName::namespace("default"));
    }

    #[test]
    fn malformed_seed_files_are_named() {
        let cases = [
            (
                r#"{"apiVersion":"v0","kind":"PricingRule","items":[]}"#,
                "apiVersion",
            ),
            (
                r#"{"apiVersion":"basable.com/v1","kind":"Order","items":[]}"#,
                "unknown config type",
            ),
            (
                r#"{"apiVersion":"basable.com/v1","kind":"PricingRule","items":[{"metadata":{"name":"x"}}]}"#,
                "metadata.namespace is required",
            ),
            (
                r#"{"apiVersion":"basable.com/v1","kind":"Namespace","items":[{"metadata":{"namespace":"a","name":"x"}}]}"#,
                "must not declare",
            ),
            (
                r#"{"apiVersion":"basable.com/v1","kind":"PricingRule","items":[{"metadata":{"namespace":"a","name":"x"},"operation":"patch"}]}"#,
                "unknown operation",
            ),
            (
                r#"{"apiVersion":"basable.com/v1","kind":"PricingRule","items":[{"metadata":{"namespace":"a","name":"x"},"spec":[]}]}"#,
                "spec must be a JSON object",
            ),
            (
                r#"{"apiVersion":"basable.com/v1","kind":"PricingRule","items":[{"metadata":{"namespace":"a","name":"x"},"extra":1}]}"#,
                "parse",
            ),
        ];
        for (text, want) in cases {
            let err = parse_seed(&types(), text, "f.json").unwrap_err();
            assert!(err.to_string().contains(want), "{err} should name {want:?}");
        }
    }
}
