//! Diagnostics with stable codes and the YAML line they point at. The codes
//! are the contract with the fixture corpus (`spec/routing/fixtures`) and
//! with the monorepo's Go validator, which runs the same corpus.

use std::fmt;

/// A diagnostic code. `E_*` fails the build; `W_*` is printed and ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Code {
    /// The file is not YAML the generator reads (syntax, a duplicate key, an
    /// alias, an empty document).
    EYaml,
    /// The document does not match the JSON Schema (`schema.json`).
    ESchema,
    /// `nanoservices` is empty.
    ENoComponents,
    /// Two nanoservices share a name.
    EDupComponent,
    /// A message appears twice in one `handles` or one `sends` list.
    EDupMessageInList,
    /// A message with a typed response has no handler.
    E1to1NoHandler,
    /// A message with a typed response has more than one handler.
    E1to1MultiHandler,
    /// Two declarations of one message disagree on `response`.
    EResponseMismatch,
    /// A `message`, `response`, `error_type`, `ctx_type` or `uses` entry is
    /// not the Rust syntax its place needs.
    ENotAType,
    /// A handled message no nanoservice sends.
    WHandlerNeverSent,
    /// A route that closes a cycle and is therefore boxed.
    WRouteCycle,
}

impl Code {
    /// The stable name.
    pub fn as_str(self) -> &'static str {
        match self {
            Code::EYaml => "E_YAML",
            Code::ESchema => "E_SCHEMA",
            Code::ENoComponents => "E_NO_COMPONENTS",
            Code::EDupComponent => "E_DUP_COMPONENT",
            Code::EDupMessageInList => "E_DUP_MESSAGE_IN_LIST",
            Code::E1to1NoHandler => "E_1TO1_NO_HANDLER",
            Code::E1to1MultiHandler => "E_1TO1_MULTI_HANDLER",
            Code::EResponseMismatch => "E_RESPONSE_MISMATCH",
            Code::ENotAType => "E_NOT_A_TYPE",
            Code::WHandlerNeverSent => "W_HANDLER_NEVER_SENT",
            Code::WRouteCycle => "W_ROUTE_CYCLE",
        }
    }

    /// Whether the code is a warning (printed, never fatal).
    pub fn is_warning(self) -> bool {
        matches!(self, Code::WHandlerNeverSent | Code::WRouteCycle)
    }

    /// Every code, for the documentation.
    pub const ALL: [Code; 11] = [
        Code::EYaml,
        Code::ESchema,
        Code::ENoComponents,
        Code::EDupComponent,
        Code::EDupMessageInList,
        Code::E1to1NoHandler,
        Code::E1to1MultiHandler,
        Code::EResponseMismatch,
        Code::ENotAType,
        Code::WHandlerNeverSent,
        Code::WRouteCycle,
    ];
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One diagnostic: a code, the line in `routing.yaml` (1-based; 0 when the
/// document has no line to point at) and the sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// The code.
    pub code: Code,
    /// The line.
    pub line: usize,
    /// The sentence.
    pub message: String,
}

impl Diagnostic {
    /// A diagnostic.
    pub fn new(code: Code, line: usize, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            code,
            line,
            message: message.into(),
        }
    }

    /// The compiler-style rendering with the file name:
    /// `routing.yaml:12: E_DUP_COMPONENT: ...`.
    pub fn render(&self, path: &str) -> String {
        format!("{path}:{}: {}: {}", self.line, self.code, self.message)
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}: {}", self.line, self.code, self.message)
    }
}

/// The diagnostics a spec failed on, in line order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostics(pub Vec<Diagnostic>);

impl fmt::Display for Diagnostics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, d) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str("\n")?;
            }
            write!(f, "{d}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Diagnostics {}
