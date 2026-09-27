"""rust_server: a Rust binary and its OCI image, the counterpart of the
basable platform's server.bzl for rules_rust. Builds the x86_64 Linux binary
in the CI's opt configuration, packages it on the digest-pinned distroless cc
base as user 65532, exposes 8080, and mints the push targets `//:push` runs.
"""

load("@rules_rust//rust:defs.bzl", "rust_binary")
load("//k8s/infra:image.bzl", "image")

def rust_server(name, srcs, deps, aliases = {}, proc_macro_deps = [], repositories = [], visibility = None):
    rust_binary(
        name = name,
        srcs = srcs,
        aliases = aliases,
        deps = deps,
        proc_macro_deps = proc_macro_deps,
        visibility = visibility,
    )
    image(
        name = name,
        binary = ":" + name,
        base = "@distroless_cc_linux_amd64",
        exposed_ports = ["8080"],
        user = "65532",
        repositories = repositories,
    )
