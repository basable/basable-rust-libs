"""resolved_protoc: the proto toolchain's protoc as a target.

The protobuf module registers its prebuilt protoc as a proto toolchain
(selected by --@protobuf//bazel/toolchains:prefer_prebuilt_protoc in
.bazelrc); `@protobuf//:protoc` would compile it from C++ instead. This rule
resolves the toolchain and forwards its compiler, so a genrule names
//tools:protoc and gets the prebuilt binary for the exec platform. A tenant
project carries the same file (the scaffolder renders it).
"""

_PROTO_TOOLCHAIN = "@protobuf//bazel/private:proto_toolchain_type"

def _resolved_protoc_impl(ctx):
    compiler = ctx.toolchains[_PROTO_TOOLCHAIN].proto.proto_compiler
    out = ctx.actions.declare_file(ctx.label.name)
    ctx.actions.symlink(output = out, target_file = compiler.executable, is_executable = True)
    return [DefaultInfo(
        executable = out,
        files = depset([out]),
        runfiles = ctx.runfiles(files = [compiler.executable]),
    )]

resolved_protoc = rule(
    implementation = _resolved_protoc_impl,
    executable = True,
    toolchains = [_PROTO_TOOLCHAIN],
)
