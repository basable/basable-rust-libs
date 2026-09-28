//! `routing.yaml` decoded and validated: the JSON Schema for structure,
//! then the coded semantic rules, every diagnostic with its line.

use crate::diagnostic::{Code, Diagnostic, Diagnostics};
use crate::graph;
use crate::yaml::{self, Node};

/// The JSON Schema (draft 2020-12) `routing.yaml` must match.
pub const SCHEMA_JSON: &str = include_str!("schema.json");

/// The `response:` value that declares a fail-fast error fan-out.
pub const RESPONSE_ERROR: &str = "error";

/// A validated `routing.yaml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    /// The `messenger:` block.
    pub messenger: Messenger,
    /// The nanoservices, in declaration order.
    pub nanoservices: Vec<Nanoservice>,
}

/// The `messenger:` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Messenger {
    /// The router's type name.
    pub name: String,
    /// The error type of every `Result` response (a Rust type path).
    pub error_type: String,
    /// The request context type (a Rust type path).
    pub ctx_type: String,
    /// The `use` trees both generated crates start with.
    pub uses: Vec<String>,
}

/// One nanoservice entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nanoservice {
    /// The name (`^[a-z][a-z0-9_]{0,63}$`).
    pub name: String,
    /// The line of the entry.
    pub line: usize,
    /// The messages it handles, in declaration order.
    pub handles: Vec<Decl>,
    /// The messages it may send, in declaration order.
    pub sends: Vec<Decl>,
}

/// One `{ message, response }` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decl {
    /// The message type path.
    pub message: String,
    /// The response: `None` (void), `Some("error")` or a typed response.
    pub response: Option<String>,
    /// The line of the declaration.
    pub line: usize,
}

/// The response kind a declaration fixes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// A strict 1:1 request answered with `Result<T, Error>`.
    Typed(String),
    /// A sequential fail-fast fan-out answered with `Result<(), Error>`.
    ErrorFanout,
    /// A void fan-out, `()`.
    Void,
}

impl Decl {
    /// The response kind.
    pub fn kind(&self) -> Kind {
        match self.response.as_deref() {
            None => Kind::Void,
            Some(RESPONSE_ERROR) => Kind::ErrorFanout,
            Some(t) => Kind::Typed(t.to_string()),
        }
    }
}

impl Nanoservice {
    /// A nanoservice with no `handles` is sends-only: no handler trait, and
    /// no route names it.
    pub fn is_sends_only(&self) -> bool {
        self.handles.is_empty()
    }
}

impl Spec {
    /// Parses and validates `text`. Every error found is returned, in line
    /// order; warnings come from [`graph::analyze`].
    pub fn parse(text: &str) -> Result<Spec, Diagnostics> {
        let doc = yaml::parse(text)
            .map_err(|e| Diagnostics(vec![Diagnostic::new(Code::EYaml, e.line, e.message)]))?;
        let schema_errors = check_schema(&doc);
        if !schema_errors.is_empty() {
            return Err(Diagnostics(sorted(schema_errors)));
        }
        let spec = decode(&doc).map_err(|d| Diagnostics(vec![d]))?;
        let mut errors = check_rust_syntax(&spec, &doc);
        errors.extend(check_semantics(&spec));
        if errors.is_empty() {
            Ok(spec)
        } else {
            Err(Diagnostics(sorted(errors)))
        }
    }
}

fn sorted(mut d: Vec<Diagnostic>) -> Vec<Diagnostic> {
    d.sort_by_key(|d| d.line);
    d
}

fn check_schema(doc: &Node) -> Vec<Diagnostic> {
    let schema: serde_json::Value =
        serde_json::from_str(SCHEMA_JSON).expect("schema.json is valid JSON");
    let validator = jsonschema::validator_for(&schema).expect("schema.json is a valid schema");
    let instance = doc.to_json();
    let mut out = Vec::new();
    for err in validator.iter_errors(&instance) {
        let pointer = err.instance_path().to_string();
        let node = doc.at(&pointer);
        let mut line = node.map(|n| n.line).unwrap_or(doc.line);
        if let jsonschema::error::ValidationErrorKind::AdditionalProperties { unexpected } =
            err.kind()
            && let Some(first) = unexpected.first()
            && let Some(key_line) = node.and_then(|n| n.key_line(first))
        {
            line = key_line;
        }
        let at = if pointer.is_empty() {
            String::new()
        } else {
            format!(" at `{pointer}`")
        };
        out.push(Diagnostic::new(Code::ESchema, line, format!("{err}{at}")));
    }
    out
}

fn decode(doc: &Node) -> Result<Spec, Diagnostic> {
    let schema_says = |what: &str, line: usize| {
        Diagnostic::new(
            Code::ESchema,
            line,
            format!("{what} (the schema check let this through)"),
        )
    };
    let messenger = doc
        .get("messenger")
        .ok_or_else(|| schema_says("no messenger", doc.line))?;
    let rust = messenger
        .get("rust")
        .ok_or_else(|| schema_says("no messenger.rust", messenger.line))?;
    let string = |node: &Node, key: &str| -> Result<String, Diagnostic> {
        node.get(key)
            .and_then(Node::as_str)
            .map(str::to_string)
            .ok_or_else(|| schema_says(&format!("`{key}` is not a string"), node.line))
    };
    let uses = match rust.get("uses") {
        None => Vec::new(),
        Some(n) => n
            .as_seq()
            .ok_or_else(|| schema_says("`uses` is not a list", n.line))?
            .iter()
            .map(|u| {
                u.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| schema_says("a `uses` entry is not a string", u.line))
            })
            .collect::<Result<_, _>>()?,
    };
    let messenger = Messenger {
        name: string(messenger, "name")?,
        error_type: string(rust, "error_type")?,
        ctx_type: string(rust, "ctx_type")?,
        uses,
    };
    let decls = |node: &Node, key: &str| -> Result<Vec<Decl>, Diagnostic> {
        match node.get(key) {
            None => Ok(Vec::new()),
            Some(list) => list
                .as_seq()
                .ok_or_else(|| schema_says(&format!("`{key}` is not a list"), list.line))?
                .iter()
                .map(|d| {
                    Ok(Decl {
                        message: string(d, "message")?,
                        response: match d.get("response") {
                            None => None,
                            Some(r) => Some(r.as_str().map(str::to_string).ok_or_else(|| {
                                schema_says("`response` is not a string", r.line)
                            })?),
                        },
                        line: d.line,
                    })
                })
                .collect(),
        }
    };
    let list = doc
        .get("nanoservices")
        .ok_or_else(|| schema_says("no nanoservices", doc.line))?;
    let nanoservices = list
        .as_seq()
        .ok_or_else(|| schema_says("`nanoservices` is not a list", list.line))?
        .iter()
        .map(|n| {
            Ok(Nanoservice {
                name: string(n, "name")?,
                line: n.line,
                handles: decls(n, "handles")?,
                sends: decls(n, "sends")?,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Spec {
        messenger,
        nanoservices,
    })
}

/// A message is a plain type path (no generics: its last segment names the
/// `handle_` / `send_` method); a response, the error type and the context
/// type are any Rust type; a `uses` entry is a `use` tree.
fn check_rust_syntax(spec: &Spec, doc: &Node) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let messenger = doc.get("messenger").expect("decoded");
    let rust = messenger.get("rust").expect("decoded");
    let type_ok = |t: &str| syn::parse_str::<syn::Type>(t).is_ok();
    if !type_ok(&spec.messenger.error_type) {
        out.push(Diagnostic::new(
            Code::ENotAType,
            rust.key_line("error_type").unwrap_or(rust.line),
            format!(
                "`error_type: {}` is not a Rust type",
                spec.messenger.error_type
            ),
        ));
    }
    if !type_ok(&spec.messenger.ctx_type) {
        out.push(Diagnostic::new(
            Code::ENotAType,
            rust.key_line("ctx_type").unwrap_or(rust.line),
            format!("`ctx_type: {}` is not a Rust type", spec.messenger.ctx_type),
        ));
    }
    if let Some(uses) = rust.get("uses").and_then(Node::as_seq) {
        for (u, node) in spec.messenger.uses.iter().zip(uses) {
            if syn::parse_str::<syn::ItemUse>(&format!("use {u};")).is_err() {
                out.push(Diagnostic::new(
                    Code::ENotAType,
                    node.line,
                    format!("`uses` entry `{u}` is not a `use` path"),
                ));
            }
        }
    }
    for n in &spec.nanoservices {
        for d in n.handles.iter().chain(&n.sends) {
            if !is_plain_path(&d.message) {
                out.push(Diagnostic::new(
                    Code::ENotAType,
                    d.line,
                    format!(
                        "`message: {}` is not a plain type path (a path without generic arguments)",
                        d.message
                    ),
                ));
            }
            if let Kind::Typed(t) = d.kind()
                && !type_ok(&t)
            {
                out.push(Diagnostic::new(
                    Code::ENotAType,
                    d.line,
                    format!("`response: {t}` is not a Rust type"),
                ));
            }
        }
    }
    out
}

/// A type path with no generic arguments: `Foo`, `messages::Foo`.
pub fn is_plain_path(s: &str) -> bool {
    match syn::parse_str::<syn::Path>(s) {
        Ok(p) => p
            .segments
            .iter()
            .all(|seg| matches!(seg.arguments, syn::PathArguments::None)),
        Err(_) => false,
    }
}

fn check_semantics(spec: &Spec) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    if spec.nanoservices.is_empty() {
        out.push(Diagnostic::new(
            Code::ENoComponents,
            0,
            "`nanoservices` declares nothing; at least one nanoservice is required",
        ));
        return out;
    }
    for (i, n) in spec.nanoservices.iter().enumerate() {
        if let Some(first) = spec.nanoservices[..i].iter().find(|m| m.name == n.name) {
            out.push(Diagnostic::new(
                Code::EDupComponent,
                n.line,
                format!(
                    "nanoservice `{}` is declared twice (first at line {})",
                    n.name, first.line
                ),
            ));
        }
        for (list, decls) in [("handles", &n.handles), ("sends", &n.sends)] {
            for (j, d) in decls.iter().enumerate() {
                if let Some(first) = decls[..j].iter().find(|e| e.message == d.message) {
                    out.push(Diagnostic::new(
                        Code::EDupMessageInList,
                        d.line,
                        format!(
                            "`{}` appears twice in the `{list}` of `{}` (first at line {})",
                            d.message, n.name, first.line
                        ),
                    ));
                }
            }
        }
    }
    if !out.is_empty() {
        // The message table below assumes unique names.
        return out;
    }
    match graph::messages(spec) {
        Ok(messages) => {
            for m in &messages {
                if let Kind::Typed(_) = m.kind {
                    if m.handlers.is_empty() {
                        let sender = m.senders.first().expect("a message is declared somewhere");
                        let decl = sender.decl(spec);
                        out.push(Diagnostic::new(
                            Code::E1to1NoHandler,
                            decl.line,
                            format!(
                                "`{}` sent by `{}` has a typed response but no nanoservice handles it",
                                m.message,
                                sender.nanoservice(spec).name
                            ),
                        ));
                    } else if m.handlers.len() > 1 {
                        let names: Vec<&str> = m
                            .handlers
                            .iter()
                            .map(|h| h.nanoservice(spec).name.as_str())
                            .collect();
                        let second = m.handlers[1].decl(spec);
                        out.push(Diagnostic::new(
                            Code::E1to1MultiHandler,
                            second.line,
                            format!(
                                "`{}` has a typed response but {} handlers ({}); a 1:1 request has exactly one",
                                m.message,
                                m.handlers.len(),
                                names.join(", ")
                            ),
                        ));
                    }
                }
            }
        }
        Err(mismatches) => out.extend(mismatches),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(text: &str) -> Vec<Code> {
        match Spec::parse(text) {
            Ok(_) => Vec::new(),
            Err(d) => d.0.into_iter().map(|d| d.code).collect(),
        }
    }

    const HEAD: &str = "version: 1\nmessenger:\n  name: AppMessenger\n  rust:\n    error_type: basable_core::AppError\n    ctx_type: basable_core::Ctx\n    uses: [\"messages::*\"]\n";

    #[test]
    fn a_minimal_valid_file_parses() {
        let spec = Spec::parse(&format!(
            "{HEAD}nanoservices:\n  - name: a\n    sends:\n      - {{ message: Ping, response: Pong }}\n  - name: b\n    handles:\n      - {{ message: Ping, response: Pong }}\n"
        ))
        .unwrap();
        assert_eq!(spec.nanoservices.len(), 2);
        assert_eq!(spec.nanoservices[0].line, 9);
        assert_eq!(spec.nanoservices[0].sends[0].line, 11);
        assert_eq!(
            spec.nanoservices[0].sends[0].kind(),
            Kind::Typed("Pong".into())
        );
        assert!(spec.nanoservices[0].is_sends_only());
    }

    #[test]
    fn schema_errors_name_the_line_of_the_offending_key() {
        let err = Spec::parse(&format!(
            "{HEAD}nanoservices:\n  - name: a\n    kind: api\n"
        ))
        .unwrap_err();
        assert_eq!(err.0.len(), 1);
        assert_eq!(err.0[0].code, Code::ESchema);
        assert_eq!(err.0[0].line, 10, "{err}");
        assert!(err.0[0].message.contains("kind"), "{err}");
    }

    #[test]
    fn the_version_is_pinned_and_the_name_pattern_holds() {
        assert_eq!(
            codes(
                "version: 2\nmessenger: {name: M, rust: {error_type: E, ctx_type: C}}\nnanoservices: [{name: a}]\n"
            ),
            vec![Code::ESchema]
        );
        assert_eq!(
            codes(&format!("{HEAD}nanoservices:\n  - name: Bad-Name\n")),
            vec![Code::ESchema]
        );
        assert_eq!(codes("- a\n"), vec![Code::ESchema]);
        assert_eq!(codes(""), vec![Code::EYaml]);
        assert_eq!(codes("a: [\n"), vec![Code::EYaml]);
    }

    #[test]
    fn the_semantic_codes_fire_where_go_did_and_where_it_did_not() {
        assert_eq!(
            codes(&format!("{HEAD}nanoservices: []\n")),
            vec![Code::ENoComponents]
        );
        assert_eq!(
            codes(&format!("{HEAD}nanoservices:\n  - name: a\n  - name: a\n")),
            vec![Code::EDupComponent]
        );
        assert_eq!(
            codes(&format!(
                "{HEAD}nanoservices:\n  - name: a\n    handles:\n      - {{ message: E }}\n      - {{ message: E }}\n"
            )),
            vec![Code::EDupMessageInList]
        );
        assert_eq!(
            codes(&format!(
                "{HEAD}nanoservices:\n  - name: a\n    sends:\n      - {{ message: Q, response: R }}\n"
            )),
            vec![Code::E1to1NoHandler]
        );
        assert_eq!(
            codes(&format!(
                "{HEAD}nanoservices:\n  - name: a\n    sends:\n      - {{ message: Q, response: R }}\n  - name: b\n    handles:\n      - {{ message: Q, response: R }}\n  - name: c\n    handles:\n      - {{ message: Q, response: R }}\n"
            )),
            vec![Code::E1to1MultiHandler]
        );
        // The Go rule: an error fan-out's handlers must also declare error.
        let err = Spec::parse(&format!(
            "{HEAD}nanoservices:\n  - name: a\n    sends:\n      - {{ message: E, response: error }}\n  - name: b\n    handles:\n      - {{ message: E }}\n"
        ))
        .unwrap_err();
        assert_eq!(err.0.len(), 1);
        assert_eq!(err.0[0].code, Code::EResponseMismatch);
        assert_eq!(err.0[0].line, 14);
        assert!(err.0[0].message.contains("line 11"), "{err}");
        // And the wider rule: any two declarations of one message agree.
        assert_eq!(
            codes(&format!(
                "{HEAD}nanoservices:\n  - name: a\n    sends:\n      - {{ message: Q, response: R }}\n  - name: b\n    handles:\n      - {{ message: Q, response: S }}\n"
            )),
            vec![Code::EResponseMismatch]
        );
    }

    #[test]
    fn rust_syntax_is_checked_per_field() {
        assert_eq!(
            codes(&format!(
                "{HEAD}nanoservices:\n  - name: a\n    handles:\n      - {{ message: \"Box<Q>\" }}\n"
            )),
            vec![Code::ENotAType]
        );
        assert_eq!(
            codes(&format!(
                "{HEAD}nanoservices:\n  - name: a\n    handles:\n      - {{ message: Q, response: \"not a type!\" }}\n"
            )),
            vec![Code::ENotAType]
        );
        assert_eq!(
            codes(
                "version: 1\nmessenger:\n  name: M\n  rust:\n    error_type: \"1 +\"\n    ctx_type: C\nnanoservices: [{name: a}]\n"
            ),
            vec![Code::ENotAType]
        );
        assert!(is_plain_path("messages::Foo"));
        assert!(!is_plain_path("Foo<T>"));
        assert!(!is_plain_path("&Foo"));
    }
}
