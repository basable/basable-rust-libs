//! The route graph: the message table (who handles and sends what, with the
//! one response kind every declaration agrees on) and the cycle analysis
//! that decides which routes are boxed.

use std::collections::BTreeSet;

use crate::diagnostic::{Code, Diagnostic};
use crate::spec::{Decl, Kind, Nanoservice, Spec};

/// A declaration's place: the nanoservice index and the index in its
/// `handles` or `sends` list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DeclRef {
    /// The index into `spec.nanoservices`.
    pub nanoservice: usize,
    /// Whether the declaration is in `sends` (else `handles`).
    pub sends: bool,
    /// The index into that list.
    pub index: usize,
}

impl DeclRef {
    /// The nanoservice.
    pub fn nanoservice<'s>(&self, spec: &'s Spec) -> &'s Nanoservice {
        &spec.nanoservices[self.nanoservice]
    }

    /// The declaration.
    pub fn decl<'s>(&self, spec: &'s Spec) -> &'s Decl {
        let n = self.nanoservice(spec);
        if self.sends {
            &n.sends[self.index]
        } else {
            &n.handles[self.index]
        }
    }
}

/// One message of the topology.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageInfo {
    /// The message type path.
    pub message: String,
    /// The response kind every declaration agrees on.
    pub kind: Kind,
    /// Its handlers, in declaration order (a fan-out calls them in this
    /// order).
    pub handlers: Vec<DeclRef>,
    /// Its senders, in declaration order.
    pub senders: Vec<DeclRef>,
}

/// The message table in first-appearance order (scanning nanoservices in
/// order, `handles` before `sends`). Fails with one `E_RESPONSE_MISMATCH`
/// per declaration that disagrees with the first.
pub fn messages(spec: &Spec) -> Result<Vec<MessageInfo>, Vec<Diagnostic>> {
    let mut table: Vec<MessageInfo> = Vec::new();
    let mut errors = Vec::new();
    for (ni, n) in spec.nanoservices.iter().enumerate() {
        let lists = [(false, &n.handles), (true, &n.sends)];
        for (sends, decls) in lists {
            for (di, d) in decls.iter().enumerate() {
                let r = DeclRef {
                    nanoservice: ni,
                    sends,
                    index: di,
                };
                let kind = d.kind();
                match table.iter_mut().find(|m| m.message == d.message) {
                    Some(m) => {
                        if m.kind != kind {
                            let first = m
                                .handlers
                                .first()
                                .or(m.senders.first())
                                .expect("a table row has a declaration");
                            errors.push(Diagnostic::new(
                                Code::EResponseMismatch,
                                d.line,
                                format!(
                                    "`{}` is declared with `{}` here but `{}` at line {} (`{}`); every declaration of a message names the same response",
                                    d.message,
                                    response_text(&kind),
                                    response_text(&m.kind),
                                    first.decl(spec).line,
                                    first.nanoservice(spec).name,
                                ),
                            ));
                        }
                        if sends {
                            m.senders.push(r);
                        } else {
                            m.handlers.push(r);
                        }
                    }
                    None => table.push(MessageInfo {
                        message: d.message.clone(),
                        kind,
                        handlers: if sends { vec![] } else { vec![r] },
                        senders: if sends { vec![r] } else { vec![] },
                    }),
                }
            }
        }
    }
    if errors.is_empty() {
        Ok(table)
    } else {
        Err(errors)
    }
}

fn response_text(kind: &Kind) -> String {
    match kind {
        Kind::Typed(t) => format!("response: {t}"),
        Kind::ErrorFanout => "response: error".to_string(),
        Kind::Void => "no response".to_string(),
    }
}

/// A route: one `(source nanoservice, message)` pair, one `Route` impl.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RouteKey {
    /// The sending nanoservice's index.
    pub source: usize,
    /// The message.
    pub message: String,
}

/// The analysed topology of a valid spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Analysis<'s> {
    /// The spec.
    pub spec: &'s Spec,
    /// The message table.
    pub messages: Vec<MessageInfo>,
    /// The routes whose future is boxed: a feedback vertex set of the route
    /// graph, chosen by depth-first search in declaration order.
    pub boxed: BTreeSet<RouteKey>,
    /// `W_ROUTE_CYCLE` and `W_HANDLER_NEVER_SENT`, in line order.
    pub warnings: Vec<Diagnostic>,
}

impl Analysis<'_> {
    /// The message row.
    pub fn message(&self, message: &str) -> &MessageInfo {
        self.messages
            .iter()
            .find(|m| m.message == message)
            .expect("every declared message has a row")
    }

    /// Every route, in declaration order (nanoservices, then their `sends`).
    pub fn routes(&self) -> Vec<RouteKey> {
        let mut out = Vec::new();
        for (ni, n) in self.spec.nanoservices.iter().enumerate() {
            for d in &n.sends {
                out.push(RouteKey {
                    source: ni,
                    message: d.message.clone(),
                });
            }
        }
        out
    }

    /// Whether the route's future is boxed.
    pub fn is_boxed(&self, route: &RouteKey) -> bool {
        self.boxed.contains(route)
    }
}

/// Analyses a spec [`Spec::parse`] accepted.
pub fn analyze(spec: &Spec) -> Analysis<'_> {
    let messages = messages(spec).expect("a parsed spec has a consistent message table");
    let mut warnings = Vec::new();
    for m in &messages {
        if m.senders.is_empty() {
            for h in &m.handlers {
                warnings.push(Diagnostic::new(
                    Code::WHandlerNeverSent,
                    h.decl(spec).line,
                    format!(
                        "`{}` handles `{}` but no nanoservice sends it",
                        h.nanoservice(spec).name,
                        m.message
                    ),
                ));
            }
        }
    }
    let boxed = feedback_set(spec, &messages, &mut warnings);
    warnings.sort_by_key(|d| d.line);
    Analysis {
        spec,
        messages,
        boxed,
        warnings,
    }
}

/// The route graph's nodes are the routes; an edge goes from a route to
/// every route one of its handlers may send. With static dispatch a
/// route's future contains its handlers' futures, which contain the routes
/// they send, so a cycle is an infinitely sized future. A depth-first walk
/// in declaration order boxes the route a back-edge points at unless the
/// cycle already passes through a boxed route.
fn feedback_set(
    spec: &Spec,
    messages: &[MessageInfo],
    warnings: &mut Vec<Diagnostic>,
) -> BTreeSet<RouteKey> {
    let mut nodes: Vec<(RouteKey, usize)> = Vec::new(); // (route, decl line)
    for (ni, n) in spec.nanoservices.iter().enumerate() {
        for d in &n.sends {
            nodes.push((
                RouteKey {
                    source: ni,
                    message: d.message.clone(),
                },
                d.line,
            ));
        }
    }
    let index_of = |source: usize, message: &str| -> usize {
        nodes
            .iter()
            .position(|(k, _)| k.source == source && k.message == message)
            .expect("every send of a nanoservice is a node")
    };
    let mut edges: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    for (i, (key, _)) in nodes.iter().enumerate() {
        let m = messages
            .iter()
            .find(|m| m.message == key.message)
            .expect("a sent message has a row");
        for h in &m.handlers {
            for d in &spec.nanoservices[h.nanoservice].sends {
                edges[i].push(index_of(h.nanoservice, &d.message));
            }
        }
    }

    #[derive(Clone, Copy, PartialEq)]
    enum State {
        New,
        Open,
        Done,
    }
    let mut state = vec![State::New; nodes.len()];
    let mut boxed: BTreeSet<usize> = BTreeSet::new();
    let mut stack: Vec<usize> = Vec::new();

    fn visit(
        v: usize,
        edges: &[Vec<usize>],
        state: &mut [State],
        stack: &mut Vec<usize>,
        boxed: &mut BTreeSet<usize>,
        cycles: &mut Vec<(usize, Vec<usize>)>,
    ) {
        state[v] = State::Open;
        stack.push(v);
        for &w in &edges[v] {
            match state[w] {
                State::New => visit(w, edges, state, stack, boxed, cycles),
                State::Open => {
                    let start = stack
                        .iter()
                        .position(|&s| s == w)
                        .expect("open nodes are on the stack");
                    let cycle = &stack[start..];
                    if !cycle.iter().any(|n| boxed.contains(n)) {
                        boxed.insert(w);
                        cycles.push((w, cycle.to_vec()));
                    }
                }
                State::Done => {}
            }
        }
        stack.pop();
        state[v] = State::Done;
    }

    let mut cycles: Vec<(usize, Vec<usize>)> = Vec::new();
    for v in 0..nodes.len() {
        if state[v] == State::New {
            visit(v, &edges, &mut state, &mut stack, &mut boxed, &mut cycles);
        }
    }
    for (w, cycle) in cycles {
        let name = |i: usize| {
            let (k, _) = &nodes[i];
            format!("{}:{}", spec.nanoservices[k.source].name, k.message)
        };
        let mut path: Vec<String> = cycle.iter().map(|&i| name(i)).collect();
        path.push(name(w));
        let (key, line) = &nodes[w];
        warnings.push(Diagnostic::new(
            Code::WRouteCycle,
            *line,
            format!(
                "the route of `{}` sent by `{}` is boxed: it closes the cycle {}",
                key.message,
                spec.nanoservices[key.source].name,
                path.join(" → ")
            ),
        ));
    }
    boxed.into_iter().map(|i| nodes[i].0.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: &str = "version: 1\nmessenger:\n  name: AppMessenger\n  rust:\n    error_type: basable_core::AppError\n    ctx_type: basable_core::Ctx\n";

    fn analysis_of(body: &str) -> (Spec, Vec<String>, Vec<Code>) {
        let spec = Spec::parse(&format!("{HEAD}{body}")).unwrap();
        let a = analyze(&spec);
        let boxed: Vec<String> = a
            .boxed
            .iter()
            .map(|k| format!("{}:{}", spec.nanoservices[k.source].name, k.message))
            .collect();
        let codes = a.warnings.iter().map(|w| w.code).collect();
        (spec.clone(), boxed, codes)
    }

    #[test]
    fn a_tree_boxes_nothing() {
        let (_, boxed, codes) = analysis_of(
            "nanoservices:\n  - name: api\n    sends:\n      - { message: A, response: R }\n  - name: a\n    handles:\n      - { message: A, response: R }\n    sends:\n      - { message: B, response: R }\n  - name: b\n    handles:\n      - { message: B, response: R }\n",
        );
        assert!(boxed.is_empty());
        assert!(codes.is_empty());
    }

    #[test]
    fn a_two_node_cycle_boxes_the_route_the_back_edge_points_at() {
        let (_, boxed, codes) = analysis_of(
            "nanoservices:\n  - name: a\n    handles:\n      - { message: Ping, response: R }\n    sends:\n      - { message: Pong, response: R }\n  - name: b\n    handles:\n      - { message: Pong, response: R }\n    sends:\n      - { message: Ping, response: R }\n",
        );
        // DFS from a:Pong → b:Ping → a:Pong (open): a:Pong is boxed.
        assert_eq!(boxed, vec!["a:Pong"]);
        assert_eq!(codes, vec![Code::WRouteCycle]);
    }

    #[test]
    fn a_self_send_is_a_cycle_of_one() {
        let (_, boxed, _) = analysis_of(
            "nanoservices:\n  - name: a\n    handles:\n      - { message: Tick }\n    sends:\n      - { message: Tick }\n",
        );
        assert_eq!(boxed, vec!["a:Tick"]);
    }

    #[test]
    fn two_cycles_through_one_route_box_once() {
        // a:X → b:Y → a:X and a:X → c:Z → a:X: boxing a:X breaks both.
        let (_, boxed, codes) = analysis_of(
            "nanoservices:\n  - name: a\n    handles:\n      - { message: Y }\n      - { message: Z }\n    sends:\n      - { message: X }\n  - name: b\n    handles:\n      - { message: X }\n    sends:\n      - { message: Y }\n  - name: c\n    handles:\n      - { message: X }\n    sends:\n      - { message: Z }\n",
        );
        assert_eq!(boxed, vec!["a:X"]);
        assert_eq!(codes.iter().filter(|c| **c == Code::WRouteCycle).count(), 1);
    }

    #[test]
    fn the_handler_never_sent_warning_names_the_handler_line() {
        let (spec, _, _) =
            analysis_of("nanoservices:\n  - name: a\n    handles:\n      - { message: Orphan }\n");
        let a = analyze(&spec);
        assert_eq!(a.warnings.len(), 1);
        assert_eq!(a.warnings[0].line, 10);
    }
}
