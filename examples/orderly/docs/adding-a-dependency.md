# Adding a crate dependency

1. Add it to `[workspace.dependencies]` in the root `Cargo.toml` with a
   version. That list is the ONLY place versions live.
2. Reference it from the crate that uses it: `name = { workspace = true }`
   in that crate's `Cargo.toml`.
3. Nothing else: BUILD files use `all_crate_deps()`, and the CI exports
   `CARGO_BAZEL_REPIN=workspace`, so the lockfiles are repinned on the
   runner. Locally: `CARGO_BAZEL_REPIN=1 bazel sync`.

Rules: never `openssl` (rustls everywhere), never a crate that needs a
system library (the image is distroless), never a second async runtime, and
never `tokio::sync::Mutex` or `RefCell` in a nanoservice crate (the clippy
aspect refuses them; `std::sync::Mutex`, atomics or `DashMap`, never held
across an `.await`).
