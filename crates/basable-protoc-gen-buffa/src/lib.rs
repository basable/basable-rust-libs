//! The protoc plugin behind `tools/proto.bzl`: `buffa-codegen` driven with
//! the platform's output shape. Upstream ships `protoc-gen-buffa` as a
//! binary-only crate, which cargo cannot list as a dependency and
//! crate_universe therefore cannot build as a `gen_binaries` tool; this
//! crate is the same plugin protocol over the same code generator, with a
//! library target so a tenant's lock carries it like `basable-messenger-gen`.
//!
//! Options (`--buffa_out=<k=v,...>:<dir>`): `views`, `json`,
//! `file_per_package`, `unknown_fields`, each `true`/`false`. The platform
//! passes `views=true,json=true,file_per_package=true`; anything else is a
//! plugin error naming the option.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::fmt;

use buffa::Message;
use buffa_codegen::CodeGenConfig;
use buffa_codegen::generated::compiler::code_generator_response::File;
use buffa_codegen::generated::compiler::{CodeGeneratorRequest, CodeGeneratorResponse};
use buffa_codegen::generated::descriptor::Edition;

/// Why a run failed; protoc prints it to the user.
#[derive(Debug)]
pub enum PluginError {
    /// The request on stdin does not decode.
    Request(String),
    /// An option is unknown or malformed.
    Option(String),
    /// Code generation failed.
    Generate(String),
}

impl fmt::Display for PluginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PluginError::Request(e) => write!(f, "protoc-gen-buffa: request does not decode: {e}"),
            PluginError::Option(e) => write!(f, "protoc-gen-buffa: {e}"),
            PluginError::Generate(e) => write!(f, "protoc-gen-buffa: {e}"),
        }
    }
}

impl std::error::Error for PluginError {}

/// Parses the plugin parameter string into the generator's configuration.
pub fn parse_options(params: &str) -> Result<CodeGenConfig, PluginError> {
    let mut config = CodeGenConfig::default();
    for param in params.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let (key, value) = param
            .split_once('=')
            .ok_or_else(|| PluginError::Option(format!("option {param:?} must be key=value")))?;
        let flag = match value.trim() {
            "true" => true,
            "false" => false,
            other => {
                return Err(PluginError::Option(format!(
                    "option {key}={other:?}: expected true or false"
                )));
            }
        };
        match key.trim() {
            "views" => config.generate_views = flag,
            "json" => config.generate_json = flag,
            "file_per_package" => config.file_per_package = flag,
            "unknown_fields" => config.preserve_unknown_fields = flag,
            other => {
                return Err(PluginError::Option(format!(
                    "unknown option {other:?} (views, json, file_per_package, unknown_fields)"
                )));
            }
        }
    }
    Ok(config)
}

/// Runs the plugin over an encoded `CodeGeneratorRequest`, returning the
/// encoded `CodeGeneratorResponse` and the generator's warnings.
pub fn run(input: &[u8]) -> Result<(Vec<u8>, Vec<String>), PluginError> {
    let request: CodeGeneratorRequest =
        buffa_codegen::decode_request(input).map_err(|e| PluginError::Request(e.to_string()))?;
    let config = parse_options(request.parameter.as_deref().unwrap_or(""))?;
    let (generated, warnings) = buffa_codegen::generate_with_diagnostics(
        &request.proto_file,
        &request.file_to_generate,
        &config,
    )
    .map_err(|e| PluginError::Generate(e.to_string()))?;
    let response = CodeGeneratorResponse {
        supported_features: Some(FEATURE_PROTO3_OPTIONAL | FEATURE_SUPPORTS_EDITIONS),
        minimum_edition: Some(Edition::EDITION_PROTO2 as i32),
        maximum_edition: Some(Edition::EDITION_2024 as i32),
        file: generated
            .into_iter()
            .map(|g| File {
                name: Some(g.name),
                content: Some(g.content),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    };
    let mut out = Vec::new();
    response.encode(&mut out);
    Ok((out, warnings.iter().map(ToString::to_string).collect()))
}

const FEATURE_PROTO3_OPTIONAL: u64 = 1;
const FEATURE_SUPPORTS_EDITIONS: u64 = 2;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_platform_options_parse_and_the_rest_are_refused() {
        let c = parse_options("views=true,json=true,file_per_package=true").unwrap();
        assert!(c.generate_views && c.generate_json && c.file_per_package);
        let c = parse_options("").unwrap();
        assert!(!c.file_per_package);
        assert!(parse_options("views").is_err());
        assert!(parse_options("views=yes").is_err());
        assert!(parse_options("reflection=true").is_err());
    }

    #[test]
    fn a_garbage_request_is_a_request_error() {
        assert!(matches!(
            run(&[0xff, 0xff, 0xff]),
            Err(PluginError::Request(_))
        ));
    }
}
