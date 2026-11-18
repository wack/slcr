# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this crate.

`slcr-gql` is the reference implementation of the read-only SLCR query
profile (`PROFILE.md`), and a member of the `slcr` workspace: it shares the
repository root's `Cargo.lock`, `rust-toolchain.toml`, `Makefile.toml`, and
CI. Run every command below from the repository root.

## Build Commands

```bash
cargo make check-format        # Format check, both packages (CI runs this)
cargo make clippy              # Lint, both packages — canonical blocking gate (== CI)
cargo make test-gql            # Run this crate's tests, including the TCK gate (== CI)
cargo make clippy-grafeo       # Lint with the grafeo feature
cargo make test-grafeo         # Run this crate's tests with grafeo, adding the differential oracle
cargo make clippy-turso        # Lint with the turso feature
cargo make test-turso          # Run this crate's tests with turso; the TCK gate runs on TursoStore
```

Always prefer `cargo make <task>` over invoking `cargo clippy`/`cargo test` directly.
The root `Makefile.toml` tasks are the canonical, `--locked` commands that CI
(`on-push.yml`, and `on-merge.yml`'s `ci-flow` / `ci-grafeo-flow` /
`ci-turso-flow`) runs, so a clean local run predicts a clean CI run.

- `cargo make test-gql` expands to `cargo test --package slcr-gql --locked`.
  Don't use cargo-nextest: the TCK runner (`tests/tck`, `harness = false`)
  has its own `main` and can't answer nextest's `--list` probe. `cargo make
  test` is `slcr`'s suite, and doesn't run this crate's.
- The Grafeo differential oracle (`tests/oracle.rs`) lives behind the `grafeo`
  Cargo feature, off by default. `cargo make clippy-grafeo` / `cargo make
  test-grafeo` run as their own parallel CI jobs (`clippy-grafeo` in
  `on-push.yml`, `grafeo` in `on-merge.yml`) so the feature doesn't rot.
- The Turso backend (`src/store/turso_store.rs`, `tests/turso.rs`) lives behind
  the `turso` feature, off by default, with the same arrangement: `cargo make
  clippy-turso` / `cargo make test-turso` (`clippy-turso` in `on-push.yml`,
  `turso` in `on-merge.yml`). `test-turso` sets `TCK_STORE=turso`, so the TCK
  baseline must hold on Turso as well as on `PetgraphStore`.
- The oracle depends on the root `Cargo.lock` holding Grafeo's sub-crates
  (`grafeo-core`, `grafeo-common`, …) at the version `Cargo.toml` pins for
  `grafeo`; a `cargo update` that moves them breaks it (see README, Testing).
- `slcr` (the root package) materializes specifications into this crate's
  stores, and its tests hold `src/query/materialize.rs` to the same graph as
  `load_spec` here. Change both together.
- `[package.metadata.dist] dist = false` keeps `slcr-query` out of `slcr`'s
  releases.
