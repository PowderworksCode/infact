# Agent Field Guide

Read this before changing the repository. Add concise entries when work reveals
a durable constraint, a non-obvious convention, or a recurring failure mode that
would help a future agent. Keep temporary plans and task-specific notes out.

## Dependencies

- Infact consumes Entl through Cargo `path` dependencies on a sibling checkout,
  so CI checks both repositories out side by side and runs with
  `working-directory: infact`. That sibling checkout is **unpinned** and tracks
  Entl's default branch, so an Entl change can break this build with nothing
  landing here. When CI fails and nothing here changed, look at Entl first.
- Straitjacket depends on this repository the same way, so a change here can
  break straitjacket's CI without anything landing there.
- `tools/discard-golden` and `tools/ts-scoreboard` each carry their own
  `Cargo.lock`. They are separate dependency surfaces from the workspace, which
  is why Dependabot covers `/tools/*` as well as `/`.

## Toolchain

- `rust-toolchain.toml` pins 1.97.1, and it is the reference the rest of the
  fleet matched rather than the other way round.
- An earlier fleet-wide pin of 1.96.1 existed only because 1.97.1 hit an
  internal compiler error building dbsp. dbsp was replaced by ascent in
  `2f97ada`, which retired both the ICE and the pin. Do not reintroduce the
  1.96.1 pin looking for a dbsp problem that no longer exists.
- `components` is listed in the toolchain file because clippy and rustfmt are
  not in the minimal profile, and their absence reports as
  `'cargo-clippy' is not installed`, which reads like a lint failure.

## Layout

- `tests/fixtures/**` is ignored through `.ordnung/overrides.toml`. Several
  crates keep TypeScript and JavaScript fixtures under `tests/`, and without
  that exclusion those crates read as TypeScript projects owing CI tasks, a type
  layer, and a fleet-managed Biome config — which would have changed what the
  fixtures exercise.

## Fleet

- `.github/dependabot.yml` is fleet-owned and comes from the `conf` repository.
  Editing it here is drift, and the next sync overwrites it.
- CI is one `gate` job: `cargo fmt --all --check`, `cargo clippy --workspace
  --all-targets -- -D warnings`, `cargo test --workspace`. Actions are pinned by
  commit SHA; do not swap one for a tag to make updating easier.
