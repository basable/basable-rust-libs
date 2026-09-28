//! `protoc-gen-buffa`: the protoc plugin protocol on stdin/stdout.

#![forbid(unsafe_code)]

use std::io::{self, Read, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    if let Some(arg) = std::env::args().nth(1) {
        match arg.as_str() {
            "--version" | "-V" => {
                println!("basable-protoc-gen-buffa {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!(
                    "protoc-gen-buffa: unexpected argument {other:?}; protoc runs this plugin over stdin"
                );
                return ExitCode::from(2);
            }
        }
    }
    let mut input = Vec::new();
    if let Err(e) = io::stdin().read_to_end(&mut input) {
        eprintln!("protoc-gen-buffa: reading stdin: {e}");
        return ExitCode::FAILURE;
    }
    match basable_protoc_gen_buffa::run(&input) {
        Ok((out, warnings)) => {
            for w in warnings {
                eprintln!("protoc-gen-buffa: warning: {w}");
            }
            let mut stdout = io::stdout().lock();
            if stdout
                .write_all(&out)
                .and_then(|()| stdout.flush())
                .is_err()
            {
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
