"""Thin wrappers so a nanoservice's BUILD.bazel never changes when a file is
added: sources are globbed, crate dependencies come from the crate's own
Cargo.toml through all_crate_deps(), and the integration tests get the
database URL from the environment.
"""

load("@crates//:defs.bzl", "aliases", "all_crate_deps")
load("@rules_rust//rust:defs.bzl", "rust_library", "rust_test")

def nanoservice_crate(name, deps = [], data = []):
    """One nanoservice crate: the library, its unit tests and its integration tests."""
    rust_library(
        name = name,
        srcs = native.glob(["src/**/*.rs"]),
        aliases = aliases(),
        compile_data = native.glob(["migrations/**"], allow_empty = True),
        crate_name = name,
        deps = all_crate_deps(normal = True) + deps,
        proc_macro_deps = all_crate_deps(proc_macro = True),
        visibility = ["//visibility:public"],
    )
    rust_test(
        name = name + "_unit_test",
        crate = ":" + name,
        deps = all_crate_deps(normal_dev = True),
        proc_macro_deps = all_crate_deps(proc_macro_dev = True),
    )
    for src in native.glob(["tests/*.rs"], allow_empty = True):
        test_name = src[len("tests/"):-len(".rs")]
        rust_test(
            name = name + "_" + test_name,
            srcs = [src],
            aliases = aliases(),
            data = data,
            deps = [":" + name] + all_crate_deps(normal = True, normal_dev = True) + deps,
            proc_macro_deps = all_crate_deps(proc_macro = True, proc_macro_dev = True),
            env_inherit = ["TEST_DATABASE_URL"],
            tags = ["requires-network"],
        )
