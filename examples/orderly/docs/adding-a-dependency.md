# Adding a crate dependency

1. Add it to `[workspace.dependencies]` in the root `Cargo.toml` with a
   version. That list is the ONLY place versions live.
2. Reference it from the crate that uses it: `name = { workspace = true }`
   in that crate's `Cargo.toml`.
3. Nothing else: BUILD files use `all_crate_deps()`, and both
   `.basable/verify.sh` and the CI export `CARGO_BAZEL_REPIN=workspace`, so
   every build repins `Cargo.lock` and `Cargo.Bazel.lock`; commit both.
   Locally, `.basable/verify.sh` does it, or on its own:
   `CARGO_BAZEL_REPIN=workspace bazel mod tidy`.

Rules: never `openssl` (rustls everywhere), never a crate that needs a
system library (the image is distroless), never a second async runtime, and
never `tokio::sync::Mutex` or `RefCell` in a nanoservice crate (the clippy
aspect refuses them; `std::sync::Mutex`, atomics or `DashMap`, never held
across an `.await`).
