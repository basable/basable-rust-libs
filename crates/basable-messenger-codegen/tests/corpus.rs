//! The fixture corpus under `spec/routing`: every valid fixture parses,
//! analyses and emits both crates as Rust that `syn` parses; every invalid
//! fixture fails with the code its first line names; the orderly goldens
//! pin the emitted text.
//!
//! To re-bless a golden after an intended change to the emitter:
//! `bazel run //crates/basable-messenger-gen -- generate --crate interfaces
//! --spec $PWD/spec/routing/fixtures/valid/orderly.yaml --output
//! $PWD/spec/routing/golden/orderly.interfaces.rs` (and `messenger`, and
//! `docs` for `orderly.topology.md`).

use std::fs;
use std::path::{Path, PathBuf};

use basable_messenger_codegen::{Crate, Spec, analyze, docs, emit};

fn corpus() -> PathBuf {
    if let (Ok(srcdir), Ok(workspace)) = (
        std::env::var("TEST_SRCDIR"),
        std::env::var("TEST_WORKSPACE"),
    ) {
        return Path::new(&srcdir).join(workspace).join("spec/routing");
    }
    if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
        return Path::new(&manifest).join("../../spec/routing");
    }
    PathBuf::from("spec/routing")
}

fn yaml_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "yaml"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no fixtures in {}", dir.display());
    files
}

#[test]
fn every_valid_fixture_parses_analyses_and_emits_parseable_rust() {
    for path in yaml_files(&corpus().join("fixtures/valid")) {
        let text = fs::read_to_string(&path).unwrap();
        let spec = Spec::parse(&text).unwrap_or_else(|d| panic!("{}:\n{d}", path.display()));
        let a = analyze(&spec);
        for which in [Crate::Interfaces, Crate::Messenger] {
            let out = emit(&a, which).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            syn::parse_file(&out)
                .unwrap_or_else(|e| panic!("{} ({}): {e}\n{out}", path.display(), which.name()));
            assert!(out.starts_with(basable_messenger_codegen::HEADER));
        }
        let md = docs::render(&a);
        assert!(md.contains("```mermaid"), "{}: {md}", path.display());
    }
}

#[test]
fn every_invalid_fixture_fails_with_the_code_its_first_line_names() {
    for path in yaml_files(&corpus().join("fixtures/invalid")) {
        let text = fs::read_to_string(&path).unwrap();
        let first = text.lines().next().unwrap_or("");
        let expected = first
            .strip_prefix("# expect: ")
            .unwrap_or_else(|| {
                panic!(
                    "{}: the first line must be `# expect: CODE`",
                    path.display()
                )
            })
            .trim();
        let err = match Spec::parse(&text) {
            Ok(_) => panic!("{}: parsed, expected {expected}", path.display()),
            Err(d) => d,
        };
        let codes: Vec<&str> = err.0.iter().map(|d| d.code.as_str()).collect();
        assert!(
            codes.contains(&expected),
            "{}: expected {expected}, got {codes:?}:\n{err}",
            path.display()
        );
        for d in &err.0 {
            assert!(
                !d.code.is_warning(),
                "{}: a warning in the error list: {d}",
                path.display()
            );
            assert!(
                d.line > 0 || d.code.as_str() == "E_NO_COMPONENTS" || d.code.as_str() == "E_YAML",
                "{}: no line: {d}",
                path.display()
            );
        }
    }
}

#[test]
fn the_expected_warnings_fire_on_the_valid_fixtures() {
    let dir = corpus().join("fixtures/valid");
    let warnings = |name: &str| -> Vec<String> {
        let text = fs::read_to_string(dir.join(name)).unwrap();
        let spec = Spec::parse(&text).unwrap();
        analyze(&spec)
            .warnings
            .iter()
            .map(|w| format!("{}:{}", w.code, w.line))
            .collect()
    };
    assert_eq!(warnings("orderly.yaml"), Vec::<String>::new());
    assert_eq!(warnings("cyclic.yaml"), vec!["W_ROUTE_CYCLE:18"]);
    assert_eq!(warnings("self_send.yaml"), vec!["W_ROUTE_CYCLE:14"]);
    assert_eq!(
        warnings("void_fanout.yaml"),
        vec!["W_HANDLER_NEVER_SENT:19"]
    );
}

#[test]
fn the_orderly_goldens_are_pinned() {
    let root = corpus();
    let text = fs::read_to_string(root.join("fixtures/valid/orderly.yaml")).unwrap();
    let spec = Spec::parse(&text).unwrap();
    let a = analyze(&spec);
    for (which, golden) in [
        (Some(Crate::Interfaces), "golden/orderly.interfaces.rs"),
        (Some(Crate::Messenger), "golden/orderly.messenger.rs"),
        (None, "golden/orderly.topology.md"),
    ] {
        let got = match which {
            Some(c) => emit(&a, c).unwrap(),
            None => docs::render(&a),
        };
        let path = root.join(golden);
        let want = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert!(
            got == want,
            "{} differs from the emitted text; re-bless it if the change is intended (see the module doc).\n--- emitted ---\n{got}",
            path.display()
        );
    }
}
