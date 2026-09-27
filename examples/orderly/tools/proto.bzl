"""buffa_connect_library: protobuf codegen as a build step.

One genrule per proto package runs the PREBUILT protoc (//tools:protoc, the
protobuf module's own) with two plugins built by crate_universe
(`gen_binaries` in MODULE.bazel): protoc-gen-buffa from the
basable-protoc-gen-buffa crate (message types, zero-copy views, JSON) and
protoc-gen-connect-rust (the Connect service traits and clients). Both run in
`file_per_package` mode, so a package yields exactly one `<dotted.package>.rs`
per plugin, in two directories; crates/proto/src/lib.rs mounts them with
`include!`, one module pair per package. Nothing generated is committed:
every build regenerates from the .proto sources, so the agent edits .proto
files and nothing else. buf is lint and breaking only.

The generated files go in the consuming rust_library's `srcs` (never
`compile_data`): rules_rust then symlinks the hand-written sources next to
them, which is what makes the relative `include!` paths resolve.
"""

def buffa_connect_library(
        name,
        protos,
        package,
        deps = [],
        connect = True,
        proto_root = "proto",
        protoc = "//tools:protoc",
        buffa_plugin = "@crates//:basable-protoc-gen-buffa__protoc-gen-buffa",
        connect_plugin = "@crates//:connectrpc-codegen__protoc-gen-connect-rust",
        buffa_module = "crate::proto",
        visibility = None):
    """Generates the buffa (and connect) file for one proto package.

    Args:
        name: the genrule name; `:<name>` is its output file(s).
        protos: the .proto files of the package, the ones code is generated for.
        package: the protobuf package the files declare (e.g. "catalog.v1").
        deps: .proto files the package imports (visible to protoc, not
            generated here).
        connect: whether the package declares a service; False skips the
            connect plugin, which emits nothing for a package without one.
        proto_root: the directory the .proto import paths are relative to,
            relative to the repository root.
        protoc: the protoc binary.
        buffa_plugin: the protoc-gen-buffa binary.
        connect_plugin: the protoc-gen-connect-rust binary.
        buffa_module: the Rust path the buffa modules are mounted at, which
            the connect stubs reference.
        visibility: visibility of the generated files.
    """
    out_dir = "generated/" + package.replace(".", "_")
    outs = [out_dir + "/buffa/" + package + ".rs"]
    plugins = [
        "--plugin=protoc-gen-buffa=$(location " + buffa_plugin + ")",
        "--buffa_opt=views=true,json=true,file_per_package=true",
        "--buffa_out=$(RULEDIR)/" + out_dir + "/buffa",
    ]
    tools = [protoc, buffa_plugin]
    dirs = "$(RULEDIR)/" + out_dir + "/buffa"
    if connect:
        outs.append(out_dir + "/connect/" + package + ".rs")
        plugins += [
            "--plugin=protoc-gen-connect-rust=$(location " + connect_plugin + ")",
            "--connect-rust_opt=file_per_package,buffa_module=" + buffa_module,
            "--connect-rust_out=$(RULEDIR)/" + out_dir + "/connect",
        ]
        tools.append(connect_plugin)
        dirs += " $(RULEDIR)/" + out_dir + "/connect"
    native.genrule(
        name = name,
        srcs = protos + deps,
        outs = outs,
        cmd = " && ".join([
            "mkdir -p " + dirs,
            " ".join(["$(location " + protoc + ")"] + plugins + ["-I " + proto_root] + ["$(location " + p + ")" for p in protos]),
        ]),
        tools = tools,
        visibility = visibility,
    )
