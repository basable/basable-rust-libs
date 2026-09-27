"""buffa_connect_library: protobuf codegen as a build step.

One genrule per proto package runs the PREBUILT protoc (//tools:protoc, the
protobuf module's own) with two plugins built by crate_universe
(`gen_binaries` in MODULE.bazel): basable-protoc-gen-buffa (message types,
zero-copy views, JSON; buffa-codegen behind the plugin protocol, because
upstream's protoc-gen-buffa is binary-only and cannot be a dependency) and
protoc-gen-connect-rust (the Connect service traits and clients). Both run in `file_per_package` mode, so a package yields exactly
one `<dotted.package>.rs` per plugin, in two directories; a hand-written
`src/lib.rs` mounts them with `include!` (see tests/connect/src/lib.rs).
Nothing generated is committed: every build regenerates from the .proto
sources, so an agent edits .proto files and nothing else.

The generated files go in the consuming rust_library's `srcs` (not
`compile_data`): rules_rust then symlinks the hand-written sources next to
them, which is what makes the relative `include!` paths resolve.
"""

def buffa_connect_library(
        name,
        protos,
        package,
        proto_root = "proto",
        protoc = "//tools:protoc",
        buffa_plugin = "@crates//:basable-protoc-gen-buffa__protoc-gen-buffa",
        connect_plugin = "@crates//:connectrpc-codegen__protoc-gen-connect-rust",
        buffa_module = "crate::proto",
        visibility = None):
    """Generates the buffa and connect files for one proto package.

    Args:
        name: the genrule name; `<name>_buffa` and `<name>_connect` are the
            two output files, `:<name>` both.
        protos: the .proto files of the package.
        package: the protobuf package the files declare (e.g. "catalog.v1").
        proto_root: the directory the .proto import paths are relative to,
            relative to the package the macro is called from.
        protoc: the protoc binary.
        buffa_plugin: the protoc-gen-buffa binary.
        connect_plugin: the protoc-gen-connect-rust binary.
        buffa_module: the Rust path the buffa file is mounted at, which the
            connect stubs reference.
        visibility: visibility of the generated files.
    """
    out_dir = "generated/" + package.replace(".", "_")
    buffa_out = out_dir + "/buffa/" + package + ".rs"
    connect_out = out_dir + "/connect/" + package + ".rs"
    include = native.package_name() + "/" + proto_root if native.package_name() else proto_root
    native.genrule(
        name = name,
        srcs = protos,
        outs = [buffa_out, connect_out],
        cmd = " && ".join([
            "mkdir -p $(RULEDIR)/" + out_dir + "/buffa $(RULEDIR)/" + out_dir + "/connect",
            " ".join([
                "$(location " + protoc + ")",
                "--plugin=protoc-gen-buffa=$(location " + buffa_plugin + ")",
                "--plugin=protoc-gen-connect-rust=$(location " + connect_plugin + ")",
                # Options through the `_opt` flags: an option value with a
                # `::` in it would split a combined `<opts>:<dir>` form.
                "--buffa_opt=views=true,json=true,file_per_package=true",
                "--buffa_out=$(RULEDIR)/" + out_dir + "/buffa",
                "--connect-rust_opt=file_per_package,buffa_module=" + buffa_module,
                "--connect-rust_out=$(RULEDIR)/" + out_dir + "/connect",
                "-I " + include,
                "$(SRCS)",
            ]),
        ]),
        tools = [protoc, buffa_plugin, connect_plugin],
        visibility = visibility,
    )
