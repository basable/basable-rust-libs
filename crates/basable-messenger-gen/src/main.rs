//! `basable-messenger-gen`: the command line over `basable-messenger-codegen`.
//!
//! ```text
//! basable-messenger-gen generate --crate interfaces|messenger --spec routing.yaml --output FILE
//! basable-messenger-gen validate --spec routing.yaml
//! basable-messenger-gen docs     --spec routing.yaml [--output FILE]
//! basable-messenger-gen schema   [--output FILE]
//! ```
//!
//! Diagnostics go to stderr as `routing.yaml:LINE: CODE: message`; an
//! `E_*` code exits 1 (the genrule fails and the build log shows the
//! file), a `W_*` code is printed and the run succeeds. `--output -` (the
//! default for `docs` and `schema`) writes to stdout.

#![forbid(unsafe_code)]

use std::fs;
use std::io::{self, Write};
use std::process::ExitCode;

use basable_messenger_codegen::{Crate, SCHEMA_JSON, Spec, analyze, docs, emit};

const USAGE: &str = "usage:
  basable-messenger-gen generate --crate interfaces|messenger --spec routing.yaml --output FILE
  basable-messenger-gen validate --spec routing.yaml
  basable-messenger-gen docs     --spec routing.yaml [--output FILE]
  basable-messenger-gen schema   [--output FILE]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure::Usage(msg)) => {
            eprintln!("{msg}\n{USAGE}");
            ExitCode::from(2)
        }
        Err(Failure::Invalid) => ExitCode::from(1),
        Err(Failure::Io(msg)) => {
            eprintln!("{msg}");
            ExitCode::from(1)
        }
    }
}

#[derive(Debug)]
enum Failure {
    Usage(String),
    /// The diagnostics were printed already.
    Invalid,
    Io(String),
}

#[derive(Default)]
struct Options {
    crate_: Option<String>,
    spec: Option<String>,
    output: Option<String>,
}

fn parse_options(args: &[String]) -> Result<Options, Failure> {
    let mut o = Options::default();
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        let value = args
            .get(i + 1)
            .ok_or_else(|| Failure::Usage(format!("{flag} needs a value")))?;
        match flag {
            "--crate" => o.crate_ = Some(value.clone()),
            "--spec" => o.spec = Some(value.clone()),
            "--output" => o.output = Some(value.clone()),
            other => return Err(Failure::Usage(format!("unknown flag {other}"))),
        }
        i += 2;
    }
    Ok(o)
}

fn run(args: &[String]) -> Result<(), Failure> {
    let (command, rest) = args
        .split_first()
        .ok_or_else(|| Failure::Usage("a command is required".to_string()))?;
    let o = parse_options(rest)?;
    match command.as_str() {
        "generate" => {
            let which = o
                .crate_
                .as_deref()
                .and_then(Crate::from_name)
                .ok_or_else(|| {
                    Failure::Usage("--crate must be interfaces or messenger".to_string())
                })?;
            let output = o
                .output
                .as_deref()
                .ok_or_else(|| Failure::Usage("--output is required".to_string()))?;
            let (path, spec) = load(o.spec.as_deref())?;
            let a = analyze(&spec);
            print_warnings(&path, &a);
            let text = emit(&a, which).map_err(|e| Failure::Io(e.to_string()))?;
            write(output, &text)
        }
        "validate" => {
            let (path, spec) = load(o.spec.as_deref())?;
            let a = analyze(&spec);
            print_warnings(&path, &a);
            let routes = a.routes().len();
            eprintln!(
                "{path}: ok ({} nanoservices, {} messages, {routes} routes, {} boxed)",
                spec.nanoservices.len(),
                a.messages.len(),
                a.boxed.len()
            );
            Ok(())
        }
        "docs" => {
            let (path, spec) = load(o.spec.as_deref())?;
            let a = analyze(&spec);
            print_warnings(&path, &a);
            write(o.output.as_deref().unwrap_or("-"), &docs::render(&a))
        }
        "schema" => write(o.output.as_deref().unwrap_or("-"), SCHEMA_JSON),
        other => Err(Failure::Usage(format!("unknown command {other}"))),
    }
}

fn load(spec: Option<&str>) -> Result<(String, Spec), Failure> {
    let path = spec.ok_or_else(|| Failure::Usage("--spec is required".to_string()))?;
    let text = fs::read_to_string(path).map_err(|e| Failure::Io(format!("{path}: {e}")))?;
    match Spec::parse(&text) {
        Ok(spec) => Ok((path.to_string(), spec)),
        Err(diagnostics) => {
            for d in &diagnostics.0 {
                eprintln!("{}", d.render(path));
            }
            Err(Failure::Invalid)
        }
    }
}

fn print_warnings(path: &str, a: &basable_messenger_codegen::Analysis<'_>) {
    for w in &a.warnings {
        eprintln!("{}", w.render(path));
    }
}

fn write(output: &str, text: &str) -> Result<(), Failure> {
    if output == "-" {
        io::stdout()
            .write_all(text.as_bytes())
            .map_err(|e| Failure::Io(e.to_string()))
    } else {
        fs::write(output, text).map_err(|e| Failure::Io(format!("{output}: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_parse_in_any_order_and_reject_the_unknown() {
        let o = parse_options(&[
            "--output".into(),
            "x".into(),
            "--spec".into(),
            "r.yaml".into(),
        ])
        .unwrap();
        assert_eq!(o.output.as_deref(), Some("x"));
        assert_eq!(o.spec.as_deref(), Some("r.yaml"));
        assert!(o.crate_.is_none());
        assert!(matches!(
            parse_options(&["--bogus".into(), "1".into()]),
            Err(Failure::Usage(_))
        ));
        assert!(matches!(
            parse_options(&["--spec".into()]),
            Err(Failure::Usage(_))
        ));
    }

    #[test]
    fn a_missing_command_or_crate_is_a_usage_error() {
        assert!(matches!(run(&[]), Err(Failure::Usage(_))));
        assert!(matches!(
            run(&["generate".into(), "--crate".into(), "other".into()]),
            Err(Failure::Usage(_))
        ));
        assert!(matches!(run(&["nope".into()]), Err(Failure::Usage(_))));
    }
}
