//! Cross-object references — `#{type:namespace:name}` in a string value of a
//! spec, `#{namespace:name}` for a namespace — their dependency graph, and
//! the order that applies every dependency before its dependents.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::error::ConfigError;
use crate::item::{Item, ItemName};
use crate::types::NAMESPACE_TYPE;

/// Parses the inner text of a `#{…}` reference. The first token is ALWAYS
/// the type, never inferred: two tokens name a namespace (root-scoped),
/// three a namespaced object.
pub(crate) fn parse_ref(inner: &str) -> Result<ItemName, ConfigError> {
    let parts: Vec<&str> = inner.split(':').collect();
    let malformed = || ConfigError::Reference {
        item: String::new(),
        reason: format!(
            "invalid reference {inner:?}: expected #{{type:namespace:name}} or #{{namespace:name}}"
        ),
    };
    match parts.as_slice() {
        [t, n] if *t == NAMESPACE_TYPE.name && !n.is_empty() => Ok(ItemName::namespace(*n)),
        [t, ns, n] if !t.is_empty() && !ns.is_empty() && !n.is_empty() => {
            Ok(ItemName::new(*t, *ns, *n))
        }
        _ => Err(malformed()),
    }
}

/// Every `#{…}` occurrence in `s`, as (start, end, inner).
fn occurrences(s: &str) -> Vec<(usize, usize, &str)> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = s[from..].find("#{") {
        let start = from + i;
        let Some(len) = s[start + 2..].find('}') else {
            break;
        };
        let end = start + 2 + len + 1;
        out.push((start, end, &s[start + 2..end - 1]));
        from = end;
    }
    out
}

/// Every distinct reference in the string values of `spec`, in encounter
/// order.
pub(crate) fn find_refs(spec: &Value) -> Result<Vec<ItemName>, ConfigError> {
    let mut refs = Vec::new();
    walk_strings(spec, &mut |s| {
        for (_, _, inner) in occurrences(s) {
            let r = parse_ref(inner)?;
            if !refs.contains(&r) {
                refs.push(r);
            }
        }
        Ok(())
    })?;
    Ok(refs)
}

/// `spec` with every reference replaced by what `resolve` returns for it
/// (an id, as a string).
pub(crate) fn resolve_refs(
    spec: &Value,
    resolve: &mut dyn FnMut(&ItemName) -> Result<String, ConfigError>,
) -> Result<Value, ConfigError> {
    map_strings(spec, &mut |s| {
        let found = occurrences(s);
        if found.is_empty() {
            return Ok(s.to_owned());
        }
        let mut out = String::with_capacity(s.len());
        let mut from = 0;
        for (start, end, inner) in found {
            out.push_str(&s[from..start]);
            out.push_str(&resolve(&parse_ref(inner)?)?);
            from = end;
        }
        out.push_str(&s[from..]);
        Ok(out)
    })
}

fn walk_strings(
    v: &Value,
    f: &mut dyn FnMut(&str) -> Result<(), ConfigError>,
) -> Result<(), ConfigError> {
    match v {
        Value::String(s) => f(s),
        Value::Array(items) => items.iter().try_for_each(|i| walk_strings(i, f)),
        Value::Object(map) => map.values().try_for_each(|i| walk_strings(i, f)),
        _ => Ok(()),
    }
}

fn map_strings(
    v: &Value,
    f: &mut dyn FnMut(&str) -> Result<String, ConfigError>,
) -> Result<Value, ConfigError> {
    Ok(match v {
        Value::String(s) => Value::String(f(s)?),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|i| map_strings(i, f))
                .collect::<Result<_, _>>()?,
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, i)| map_strings(i, f).map(|m| (k.clone(), m)))
                .collect::<Result<_, _>>()?,
        ),
        other => other.clone(),
    })
}

/// The dependencies of an item: its namespace (if any) plus every reference
/// target. A malformed reference is surfaced by `validate_deps`; here it
/// contributes nothing.
pub(crate) fn item_deps(item: &Item) -> Vec<ItemName> {
    let mut deps = Vec::new();
    if !item.name.namespace.is_empty() {
        deps.push(ItemName::namespace(item.name.namespace.clone()));
    }
    if let Ok(refs) = find_refs(&item.spec) {
        deps.extend(refs);
    }
    deps
}

/// Verifies every reference parses and points at an object declared in the
/// same file set — a config run is self-contained: EVERY dependency, each
/// `#{…}` cross-reference AND the object's containment namespace, must be
/// declared alongside. You cannot reference, or place an object into,
/// something that exists only in the database. Checked on the in-memory
/// graph before any transaction, so a dangling reference fails fast with
/// full context.
pub(crate) fn validate_deps(items: &BTreeMap<ItemName, Item>) -> Result<(), ConfigError> {
    for (name, item) in items {
        let at = |reason: String| ConfigError::Reference {
            item: format!("{name} ({})", item.source_file),
            reason,
        };
        if let Err(ConfigError::Reference { reason, .. }) = find_refs(&item.spec) {
            return Err(at(reason));
        }
        for dep in item_deps(item) {
            if !items.contains_key(&dep) {
                return Err(at(format!(
                    "requires {dep}, which is not declared in the config files"
                )));
            }
        }
    }
    Ok(())
}

/// Orders the items so that every dependency precedes its dependents.
/// Dependencies absent from `items` add no edge. A cycle is an error naming
/// its path. Deterministic: the map iterates in key order.
pub(crate) fn topo_sort(items: &BTreeMap<ItemName, Item>) -> Result<Vec<&Item>, ConfigError> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        White,
        Grey,
        Black,
    }
    fn visit<'a>(
        items: &'a BTreeMap<ItemName, Item>,
        name: &ItemName,
        marks: &mut BTreeMap<ItemName, Mark>,
        stack: &mut Vec<ItemName>,
        order: &mut Vec<&'a Item>,
    ) -> Result<(), ConfigError> {
        match marks.get(name).copied().unwrap_or(Mark::White) {
            Mark::Black => return Ok(()),
            Mark::Grey => {
                let path: Vec<String> = stack.iter().map(ToString::to_string).collect();
                return Err(ConfigError::Cycle(format!(
                    "{} -> {name}",
                    path.join(" -> ")
                )));
            }
            Mark::White => {}
        }
        let Some(item) = items.get(name) else {
            return Ok(());
        };
        marks.insert(name.clone(), Mark::Grey);
        stack.push(name.clone());
        for dep in item_deps(item) {
            visit(items, &dep, marks, stack, order)?;
        }
        stack.pop();
        marks.insert(name.clone(), Mark::Black);
        order.push(item);
        Ok(())
    }

    let mut marks = BTreeMap::new();
    let mut order = Vec::with_capacity(items.len());
    for name in items.keys() {
        visit(items, name, &mut marks, &mut Vec::new(), &mut order)?;
    }
    Ok(order)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::Operation;
    use crate::types::TypeInfo;

    const PRICING: TypeInfo = TypeInfo {
        id: 2,
        name: "pricing_configuration",
        prefix: "price",
    };

    fn item(t: TypeInfo, ns: &str, name: &str, raw: &str) -> Item {
        Item {
            name: ItemName::new(t.name, ns, name),
            type_info: t,
            operation: Operation::Store,
            labels: Default::default(),
            spec: serde_json::from_str(raw).unwrap(),
            source_file: "test.json".into(),
        }
    }

    fn namespace(name: &str) -> Item {
        item(NAMESPACE_TYPE, "", name, "{}")
    }

    fn items(list: Vec<Item>) -> BTreeMap<ItemName, Item> {
        list.into_iter().map(|i| (i.name.clone(), i)).collect()
    }

    fn index_of(ordered: &[&Item], name: &ItemName) -> usize {
        ordered.iter().position(|i| &i.name == name).unwrap()
    }

    /// A self-contained graph (namespace + two pricing rows, one referencing
    /// the other) passes validation and topo-sorts with every dependency
    /// before its dependents.
    #[test]
    fn a_valid_graph_orders_dependencies_first() {
        let ns = namespace("billing");
        let base = item(PRICING, "billing", "default-initial", "{}");
        let next = item(
            PRICING,
            "billing",
            "default-next",
            r##"{"supersedes":"#{pricing_configuration:billing:default-initial}"}"##,
        );
        let (ns_n, base_n, next_n) = (ns.name.clone(), base.name.clone(), next.name.clone());
        let m = items(vec![ns, base, next]);
        validate_deps(&m).unwrap();
        let ordered = topo_sort(&m).unwrap();
        let (a, b, c) = (
            index_of(&ordered, &ns_n),
            index_of(&ordered, &base_n),
            index_of(&ordered, &next_n),
        );
        assert!(a < b && b < c, "topo order wrong: ns={a} base={b} next={c}");
    }

    /// A reference to an object not declared in the files is rejected — you
    /// cannot reference a DB-only object.
    #[test]
    fn a_dangling_reference_is_rejected() {
        let ns = namespace("billing");
        let next = item(
            PRICING,
            "billing",
            "default-next",
            r##"{"supersedes":"#{pricing_configuration:billing:ghost}"}"##,
        );
        let err = validate_deps(&items(vec![ns, next]))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("ghost") && err.contains("not declared"),
            "{err}"
        );
    }

    /// A config run is self-contained: an object placed in a namespace must
    /// declare that namespace in the same file set.
    #[test]
    fn a_missing_namespace_is_rejected() {
        let pricing = item(PRICING, "billing", "default-initial", "{}");
        let err = validate_deps(&items(vec![pricing]))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("billing") && err.contains("not declared"),
            "{err}"
        );
    }

    /// A namespace can be referenced like any other object via the two-part
    /// form; it is found, required, and ordered before its referrer.
    #[test]
    fn a_namespace_reference_is_a_dependency() {
        let ns = namespace("billing");
        let pricing = item(
            PRICING,
            "billing",
            "default-initial",
            r##"{"homeNamespace":"#{namespace:billing}"}"##,
        );
        let (ns_n, p_n) = (ns.name.clone(), pricing.name.clone());
        let m = items(vec![ns, pricing.clone()]);
        validate_deps(&m).unwrap();
        let ordered = topo_sort(&m).unwrap();
        assert!(index_of(&ordered, &ns_n) < index_of(&ordered, &p_n));
        assert!(validate_deps(&items(vec![pricing])).is_err());
    }

    /// A reference cycle is rejected by the topological sort.
    #[test]
    fn a_cycle_is_rejected_at_ordering() {
        let ns = namespace("billing");
        let a = item(
            PRICING,
            "billing",
            "a",
            r##"{"x":"#{pricing_configuration:billing:b}"}"##,
        );
        let b = item(
            PRICING,
            "billing",
            "b",
            r##"{"x":"#{pricing_configuration:billing:a}"}"##,
        );
        let m = items(vec![ns, a, b]);
        validate_deps(&m).unwrap();
        let err = topo_sort(&m).unwrap_err().to_string();
        assert!(err.contains("circular"), "{err}");
    }

    #[test]
    fn references_parse_and_resolve_inside_strings() {
        assert_eq!(
            parse_ref("namespace:billing").unwrap(),
            ItemName::namespace("billing")
        );
        assert_eq!(
            parse_ref("unit:billing:eur").unwrap(),
            ItemName::new("unit", "billing", "eur")
        );
        assert!(parse_ref("just-a-name").is_err());
        assert!(parse_ref("a:b:c:d").is_err());
        assert!(parse_ref("::").is_err());

        let spec = serde_json::json!({
            "unit": "#{unit:billing:eur}",
            "nested": {"list": ["#{unit:billing:eur}", "#{namespace:billing}", "plain"]},
            "label": "id=#{unit:billing:usd}/x",
            "n": 3
        });
        let refs = find_refs(&spec).unwrap();
        assert_eq!(
            refs,
            vec![
                ItemName::new("unit", "billing", "usd"),
                ItemName::new("unit", "billing", "eur"),
                ItemName::namespace("billing"),
            ],
            "distinct, in traversal order (object keys sorted)"
        );
        let resolved = resolve_refs(&spec, &mut |r| Ok(format!("<{}>", r.name))).unwrap();
        assert_eq!(
            resolved,
            serde_json::json!({
                "unit": "<eur>",
                "nested": {"list": ["<eur>", "<billing>", "plain"]},
                "label": "id=<usd>/x",
                "n": 3
            })
        );
        let err = find_refs(&serde_json::json!({"x": "#{bad}"})).unwrap_err();
        assert!(err.to_string().contains("invalid reference"), "{err}");
    }
}
