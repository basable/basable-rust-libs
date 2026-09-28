//! A YAML document as a tree that remembers the line of every node. The
//! generator's diagnostics name the line in `routing.yaml`, which the
//! serde-style decoders throw away, so the file is read through the
//! event API and kept as this small tree; the JSON Schema check runs over
//! its JSON rendering and maps each error's instance path back to a line.

use yaml_rust2::parser::{Event, MarkedEventReceiver, Parser};
use yaml_rust2::scanner::{Marker, TScalarStyle};

/// One YAML node with the 1-based line it starts on.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    /// The line the node starts on (1-based).
    pub line: usize,
    /// The node's value.
    pub value: Value,
}

/// A YAML value: the scalar kinds the file uses, sequences and mappings.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// `null`, `~` or an empty plain scalar.
    Null,
    /// A plain `true` / `false`.
    Bool(bool),
    /// A plain integer.
    Int(i64),
    /// Any other scalar (quoted scalars are always strings).
    Str(String),
    /// A sequence.
    Seq(Vec<Node>),
    /// A mapping, in document order; keys are unique.
    Map(Vec<Entry>),
}

/// One mapping entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// The key.
    pub key: String,
    /// The line the key is on.
    pub line: usize,
    /// The value.
    pub value: Node,
}

/// A document that does not parse, with the line the parser stopped at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YamlError {
    /// The line (1-based; 0 when the document is empty).
    pub line: usize,
    /// What went wrong.
    pub message: String,
}

impl std::fmt::Display for YamlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for YamlError {}

/// Parses the first document of `text`.
pub fn parse(text: &str) -> Result<Node, YamlError> {
    let mut parser = Parser::new_from_str(text);
    let mut builder = Builder::default();
    parser.load(&mut builder, false).map_err(|e| YamlError {
        line: e.marker().line(),
        message: e.info().to_string(),
    })?;
    if let Some(err) = builder.error {
        return Err(err);
    }
    builder.root.ok_or(YamlError {
        line: 0,
        message: "the document is empty".to_string(),
    })
}

impl Node {
    /// The node at a JSON pointer (`/nanoservices/0/handles`), if any.
    pub fn at(&self, pointer: &str) -> Option<&Node> {
        let mut node = self;
        for segment in pointer.split('/').skip(1) {
            let segment = segment.replace("~1", "/").replace("~0", "~");
            node = match &node.value {
                Value::Map(entries) => &entries.iter().find(|e| e.key == segment)?.value,
                Value::Seq(items) => items.get(segment.parse::<usize>().ok()?)?,
                _ => return None,
            };
        }
        Some(node)
    }

    /// The line of `key` in a mapping node.
    pub fn key_line(&self, key: &str) -> Option<usize> {
        match &self.value {
            Value::Map(entries) => entries.iter().find(|e| e.key == key).map(|e| e.line),
            _ => None,
        }
    }

    /// The value of `key` in a mapping node.
    pub fn get(&self, key: &str) -> Option<&Node> {
        match &self.value {
            Value::Map(entries) => entries.iter().find(|e| e.key == key).map(|e| &e.value),
            _ => None,
        }
    }

    /// The string of a string node.
    pub fn as_str(&self) -> Option<&str> {
        match &self.value {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The items of a sequence node.
    pub fn as_seq(&self) -> Option<&[Node]> {
        match &self.value {
            Value::Seq(items) => Some(items),
            _ => None,
        }
    }

    /// The JSON rendering, for the schema check.
    pub fn to_json(&self) -> serde_json::Value {
        match &self.value {
            Value::Null => serde_json::Value::Null,
            Value::Bool(b) => serde_json::Value::Bool(*b),
            Value::Int(i) => serde_json::Value::from(*i),
            Value::Str(s) => serde_json::Value::String(s.clone()),
            Value::Seq(items) => {
                serde_json::Value::Array(items.iter().map(Node::to_json).collect())
            }
            Value::Map(entries) => serde_json::Value::Object(
                entries
                    .iter()
                    .map(|e| (e.key.clone(), e.value.to_json()))
                    .collect(),
            ),
        }
    }
}

enum Frame {
    Seq {
        line: usize,
        items: Vec<Node>,
    },
    Map {
        line: usize,
        entries: Vec<Entry>,
        pending_key: Option<(String, usize)>,
    },
}

#[derive(Default)]
struct Builder {
    stack: Vec<Frame>,
    root: Option<Node>,
    error: Option<YamlError>,
}

impl Builder {
    fn fail(&mut self, line: usize, message: impl Into<String>) {
        if self.error.is_none() {
            self.error = Some(YamlError {
                line,
                message: message.into(),
            });
        }
    }

    fn add(&mut self, node: Node) {
        match self.stack.last_mut() {
            None => {
                if self.root.is_none() {
                    self.root = Some(node);
                }
            }
            Some(Frame::Seq { items, .. }) => items.push(node),
            Some(Frame::Map {
                entries,
                pending_key,
                ..
            }) => match pending_key.take() {
                Some((key, line)) => {
                    if entries.iter().any(|e| e.key == key) {
                        let msg = format!("duplicate key `{key}`");
                        self.fail(line, msg);
                        return;
                    }
                    entries.push(Entry {
                        key,
                        line,
                        value: node,
                    });
                }
                None => {
                    // A mapping key that is not a scalar.
                    let line = node.line;
                    self.fail(line, "a mapping key must be a scalar");
                }
            },
        }
    }
}

fn scalar(text: String, style: TScalarStyle) -> Value {
    if style != TScalarStyle::Plain {
        return Value::Str(text);
    }
    match text.as_str() {
        "" | "~" | "null" | "Null" | "NULL" => Value::Null,
        "true" | "True" | "TRUE" => Value::Bool(true),
        "false" | "False" | "FALSE" => Value::Bool(false),
        _ => match text.parse::<i64>() {
            Ok(i) => Value::Int(i),
            Err(_) => Value::Str(text),
        },
    }
}

impl MarkedEventReceiver for Builder {
    fn on_event(&mut self, ev: Event, mark: Marker) {
        if self.error.is_some() {
            return;
        }
        let line = mark.line();
        match ev {
            Event::Scalar(text, style, _, _) => {
                let wants_key = matches!(
                    self.stack.last(),
                    Some(Frame::Map {
                        pending_key: None,
                        ..
                    })
                );
                if wants_key {
                    if let Some(Frame::Map { pending_key, .. }) = self.stack.last_mut() {
                        *pending_key = Some((text, line));
                    }
                } else {
                    self.add(Node {
                        line,
                        value: scalar(text, style),
                    });
                }
            }
            Event::SequenceStart(..) => self.stack.push(Frame::Seq {
                line,
                items: Vec::new(),
            }),
            Event::MappingStart(..) => self.stack.push(Frame::Map {
                line,
                entries: Vec::new(),
                pending_key: None,
            }),
            Event::SequenceEnd | Event::MappingEnd => match self.stack.pop() {
                Some(Frame::Seq { line, items }) => self.add(Node {
                    line,
                    value: Value::Seq(items),
                }),
                Some(Frame::Map {
                    line,
                    entries,
                    pending_key,
                }) => {
                    if let Some((key, line)) = pending_key {
                        let msg = format!("key `{key}` has no value");
                        self.fail(line, msg);
                        return;
                    }
                    self.add(Node {
                        line,
                        value: Value::Map(entries),
                    })
                }
                None => self.fail(line, "unbalanced end of collection"),
            },
            Event::Alias(_) => self.fail(line, "YAML anchors and aliases are not supported"),
            Event::Nothing
            | Event::StreamStart
            | Event::StreamEnd
            | Event::DocumentStart
            | Event::DocumentEnd => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nodes_remember_their_lines_and_the_pointer_walk_finds_them() {
        let doc = parse("version: 1\nnanoservices:\n  - name: a\n    handles:\n      - { message: M, response: R }\n  - name: b\n")
            .unwrap();
        assert_eq!(doc.line, 1);
        assert_eq!(doc.key_line("nanoservices"), Some(2));
        let b = doc.at("/nanoservices/1").unwrap();
        assert_eq!(b.line, 6);
        let response = doc.at("/nanoservices/0/handles/0/response").unwrap();
        assert_eq!(response.line, 5);
        assert_eq!(response.as_str(), Some("R"));
        assert_eq!(doc.at("/version").unwrap().value, Value::Int(1));
        assert!(doc.at("/nanoservices/7").is_none());
    }

    #[test]
    fn quoted_scalars_stay_strings_and_plain_ones_are_typed() {
        let doc = parse("a: '1'\nb: 1\nc: true\nd: ~\ne: \"true\"\n").unwrap();
        assert_eq!(doc.get("a").unwrap().value, Value::Str("1".into()));
        assert_eq!(doc.get("b").unwrap().value, Value::Int(1));
        assert_eq!(doc.get("c").unwrap().value, Value::Bool(true));
        assert_eq!(doc.get("d").unwrap().value, Value::Null);
        assert_eq!(doc.get("e").unwrap().value, Value::Str("true".into()));
    }

    #[test]
    fn a_duplicate_key_an_alias_and_a_syntax_error_name_their_line() {
        let dup = parse("a: 1\nb: 2\na: 3\n").unwrap_err();
        assert_eq!(dup.line, 3);
        assert!(dup.message.contains("duplicate key `a`"), "{dup}");
        let alias = parse("a: &x 1\nb: *x\n").unwrap_err();
        assert_eq!(alias.line, 2);
        let bad = parse("a: [1, 2\nb: 3\n").unwrap_err();
        assert!(bad.line >= 1, "{bad}");
        assert_eq!(parse("").unwrap_err().line, 0);
    }
}
