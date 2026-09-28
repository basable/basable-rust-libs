# Shop

A basable nanoservice application, scaffolded from the plan under
`docs/01-shop/`. See `AGENTS.md` for how the repository is
organised and `docs/DIRECTIVE.md` for the rules every nanoservice follows.

- Build and test: `bazel test //...`
- Deploy: push to `dev` (preview) or `main` (live); the workflow under
  `.github/workflows/` builds the images and uploads the rendered
  manifests the platform applies.
