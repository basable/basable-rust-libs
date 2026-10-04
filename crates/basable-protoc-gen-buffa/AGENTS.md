# basable-protoc-gen-buffa — the message codegen plugin

The protoc plugin `tools/proto.bzl` runs for a tenant's message types:
`buffa-codegen` behind the protoc plugin protocol with the platform's fixed
output shape. It exists because upstream ships `protoc-gen-buffa` as a
binary-only crate, which cargo cannot list as a dependency and
crate_universe therefore cannot build as a `gen_binaries` tool; this crate
is the same generator with a library target, so a tenant's lock carries it
like `basable-messenger-gen` (`docs/porting-notes.md` 72 and 76). There is
no Go original: the monorepo generates Go from protos with `protoc-gen-go`.
The Directive (`docs/DIRECTIVE.md` in every tenant repository,
`golang/controller/lib/scaffold/directive.md` in the monorepo) is the
contract this crate serves: protobuf exists only at the API boundary, and
the `proto` crate is generated, never edited.

## The plugin

| Item | What it does |
|---|---|
| `protoc-gen-buffa` (binary) | The protoc plugin protocol on stdin/stdout: a `CodeGeneratorRequest` in, a `CodeGeneratorResponse` out; `--version` prints the crate version; any other argument is a usage error (exit 2), because protoc runs the plugin over stdin |
| `run(input) -> (output, warnings)` | The library call the binary wraps: decode the request, parse its parameter string, generate, encode. Warnings go to stderr prefixed `protoc-gen-buffa: warning:` |
| `parse_options(params) -> CodeGenConfig` | `<k=v,...>`: `views`, `json`, `file_per_package`, `unknown_fields`, each `true` or `false`; an unknown option or a value that is not `true`/`false` is `PluginError::Option` naming it |
| `PluginError` | `Request` (stdin does not decode), `Option`, `Generate`; `Display` is what protoc prints to the user |

The platform passes `views=true,json=true,file_per_package=true`:

- `views`: zero-copy view types beside the owned messages;
- `json`: protobuf-JSON `Serialize`/`Deserialize` through serde, which is
  what `basable-config` reads seed files through and what the JSON codec of
  Connect uses;
- `file_per_package`: exactly one `<dotted.package>.rs` per package, so the
  genrule can declare its outputs without per-file stitchers;
- `unknown_fields` is off.

## How a tenant runs it

`tools/proto.bzl`'s `buffa_connect_library(name, protos, package, ..)`
runs the protobuf module's prebuilt protoc with this plugin
(`@crates//:basable-protoc-gen-buffa__protoc-gen-buffa`) and
`protoc-gen-connect-rust` (from `connectrpc-codegen`), one genrule per proto
package. Options travel as `--buffa_opt=views=true,json=true,file_per_package=true`
rather than inside `--buffa_out=<opts>:<dir>`, because an option value
containing `::` (the connect plugin's `buffa_module=crate::proto`) would
split the combined form at the wrong colon. The crate holding the generated
messages depends on `serde` itself, since the JSON impls derive from it.

## Tests

Unit tests in `src/lib.rs` pin the option parser (the platform's string,
each flag independently, the malformed forms). The generated output is
exercised end to end in `tests/connect`, where `echo.proto` goes through
the macro and the generated client round-trips in both codecs.
