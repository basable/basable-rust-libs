//! The topology documentation: a table of nanoservices and messages and a
//! mermaid diagram, for the scaffolder to place in `docs/architecture.md`.

use std::fmt::Write;

use crate::graph::Analysis;
use crate::spec::Kind;

/// Renders the topology as Markdown.
pub fn render(a: &Analysis<'_>) -> String {
    let mut out = String::new();
    let spec = a.spec;
    let _ = writeln!(out, "## Message topology");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Generated from `routing.yaml` by `basable-messenger-gen docs`; the router is `{}`.",
        spec.messenger.name
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "| Nanoservice | Handles | Sends |");
    let _ = writeln!(out, "|---|---|---|");
    for n in &spec.nanoservices {
        let handles: Vec<String> = n
            .handles
            .iter()
            .map(|d| decl_text(&d.message, &d.kind()))
            .collect();
        let sends: Vec<String> = n
            .sends
            .iter()
            .map(|d| decl_text(&d.message, &d.kind()))
            .collect();
        let role = if n.is_sends_only() {
            " (sends-only)"
        } else {
            ""
        };
        let _ = writeln!(
            out,
            "| `{}`{role} | {} | {} |",
            n.name,
            or_dash(&handles),
            or_dash(&sends)
        );
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "| Message | Response | Shape | Handlers | Senders |");
    let _ = writeln!(out, "|---|---|---|---|---|");
    for m in &a.messages {
        let handlers: Vec<String> = m
            .handlers
            .iter()
            .map(|h| format!("`{}`", h.nanoservice(spec).name))
            .collect();
        let senders: Vec<String> = m
            .senders
            .iter()
            .map(|s| format!("`{}`", s.nanoservice(spec).name))
            .collect();
        let (response, shape) = match &m.kind {
            Kind::Typed(t) => (format!("`{t}`"), "1:1 request".to_string()),
            Kind::ErrorFanout => ("`error`".to_string(), "fail-fast error fan-out".to_string()),
            Kind::Void => ("—".to_string(), "void fan-out".to_string()),
        };
        let _ = writeln!(
            out,
            "| `{}` | {response} | {shape} | {} | {} |",
            m.message,
            or_dash(&handlers),
            or_dash(&senders)
        );
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "```mermaid");
    let _ = writeln!(out, "graph LR");
    for n in &spec.nanoservices {
        let _ = writeln!(out, "    {}[{}]", n.name, n.name);
    }
    for key in a.routes() {
        let source = &spec.nanoservices[key.source].name;
        let m = a.message(&key.message);
        let boxed = if a.is_boxed(&key) { " (boxed)" } else { "" };
        if m.handlers.is_empty() {
            let _ = writeln!(out, "    {source} -.->|{}{boxed}| nobody", key.message);
        }
        for h in &m.handlers {
            let arrow = match m.kind {
                Kind::Typed(_) => "-->",
                Kind::ErrorFanout | Kind::Void => "-.->",
            };
            let _ = writeln!(
                out,
                "    {source} {arrow}|{}{boxed}| {}",
                key.message,
                h.nanoservice(spec).name
            );
        }
    }
    let _ = writeln!(out, "```");
    if !a.warnings.is_empty() {
        let _ = writeln!(out);
        for w in &a.warnings {
            let _ = writeln!(out, "- `{}` (line {}): {}", w.code, w.line, w.message);
        }
    }
    out
}

fn decl_text(message: &str, kind: &Kind) -> String {
    match kind {
        Kind::Typed(t) => format!("`{message}` → `{t}`"),
        Kind::ErrorFanout => format!("`{message}` → error"),
        Kind::Void => format!("`{message}`"),
    }
}

fn or_dash(items: &[String]) -> String {
    if items.is_empty() {
        "—".to_string()
    } else {
        items.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::analyze;
    use crate::spec::Spec;

    #[test]
    fn the_topology_lists_every_nanoservice_message_and_edge() {
        let spec = Spec::parse(
            "version: 1\nmessenger:\n  name: M\n  rust:\n    error_type: E\n    ctx_type: C\nnanoservices:\n  - name: api\n    sends:\n      - { message: Q, response: R }\n  - name: a\n    handles:\n      - { message: Q, response: R }\n",
        )
        .unwrap();
        let a = analyze(&spec);
        let md = render(&a);
        assert!(
            md.contains("| `api` (sends-only) | — | `Q` → `R` |"),
            "{md}"
        );
        assert!(
            md.contains("| `Q` | `R` | 1:1 request | `a` | `api` |"),
            "{md}"
        );
        assert!(md.contains("    api -->|Q| a"), "{md}");
    }
}
