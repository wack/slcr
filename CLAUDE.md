# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

# Build Commands

```bash
cargo build --release          # Production build
cargo make check-format        # Format check (CI runs this)
cargo make clippy              # Lint — canonical blocking gate (== CI and bacon)
cargo make test                # Run all tests (== CI's nextest run)
cargo nextest run --locked <filter>   # Run a single test / matching tests by name substring
cargo make monitor             # Headless bacon for Claude Code's Monitor tool
```

Always prefer `cargo make <task>` over invoking `cargo clippy`/`cargo test` directly.
The `Makefile.toml` tasks are the canonical, `--locked` commands that CI
(`on-push.yml`, `on-merge.yml`'s `ci-flow`) and bacon (`cargo make monitor`) all
share byte-for-byte, so a clean local run predicts a clean CI run. Plain `cargo
clippy`/`cargo test` skip `--locked` and can silently pass locally on a resolution
CI would reject.

- `cargo make clippy` expands to `cargo clippy --all-targets --workspace --locked
  -- -D warnings` (see the `[tasks.clippy]` comment in `Makefile.toml`).
- `cargo make test` expands to `cargo nextest run --locked` — CI's `ci-flow` uses
  nextest, not the built-in test harness, so `cargo test` alone doesn't fully
  match CI.
- `on-push.yml` only runs format and clippy; the test suite (and coverage) runs in
  `on-merge.yml`'s `ci-flow`.

The toolchain is pinned in `rust-toolchain.toml`; the crate is edition 2024 and
forbids `unsafe_code`.

# Architecture

`slcr` is a CLI for SLCR specifications: requirements documents stored as a graph
and serialized as YAML, JSON, or TOML. A single crate exposes a library
(`src/lib.rs`) and a thin binary (`src/bin/main.rs`). All tests are inline
`#[cfg(test)]` modules; `tests/fixtures/` holds data only (the canonical
`todo-api` spec in all three formats, loaded via `include_str!`).

## Command flow

`main` parses `config::Cli` (clap derive), builds a `Terminal`, installs the miette
error hook, then calls `SlcrCommand::dispatch` (`src/config/command.rs`), which
constructs a per-command struct from `src/cli/` and calls its synchronous
`dispatch(self) -> miette::Result<()>`. To add a command: add a variant to
`SlcrCommand`, a struct in `src/cli/` that holds the `Terminal`, and a match arm.

- `render` is a stub returning `cli::NotImplemented`.
- `init [FILE]` creates a new specification (`SlcrRequirementsDocument::new`:
  root `SEC-001`, Glossary `SEC-002`) at `FILE`, by default the well-known
  `SPEC.slcr.yml`. Without `--name`, the spec is named after the file's stem
  minus any `.slcr`/`.spec`, or, for `SPEC.slcr.*`, after its directory; an
  invalid result is an error, never slugified. `--title` defaults to the name.
  It writes through `FileSystem::create_file`, so it never overwrites anything
  (even a dangling symlink) and never creates a missing directory.
- `check <FILE>...` loads each requirements file with
  `RequirementsFile::unchecked()`, writes every finding to stderr as a miette
  diagnostic (`Terminal::write_diagnostic`) and each file's status to stdout,
  and fails with `CheckFailed` if any file can't be loaded or breaks an
  invariant (or has warnings, under `--deny-warnings`). `--format json` writes
  one JSON document to stdout instead (`src/cli/check/json.rs`); findings
  serialize with a kebab-case `code` plus their fields, so renaming a
  `Violation` or `Warning` variant or field changes that output. Validator-file
  checks wait on the validator-file schema.
- Commands stay synchronous; an async command calls `cli::block_on` from its
  `dispatch()` (see `src/cli/runtime.rs`). Remove the `#[expect(unused_imports)]`
  on its re-export in `src/cli/mod.rs` when the first caller lands.
- All user-facing output goes through `Terminal` (`src/terminal/`), which owns
  color detection (`--enable-colors`) and logging setup (`--log-level` / `LOG_LEVEL`).
- Errors are `thiserror` + `miette::Diagnostic` types; use `#[diagnostic(help(...))]`
  for remediation hints rather than ad-hoc messages.

## Specification graph (`src/spec/`)

The data model mirrors the specification graph JSON Schema (draft 2020-12) kept
on the "SLCR Schemas" page in Notion — that page, not this repo, is the source of
truth for field names, grammar, and the `x-checkInvariants` list.

- `graph.rs` — `SlcrRequirementsDocument`: a `root` `Section` containment tree plus a separate
  `glossary` of `Term`s. Cross edges (`refines`, `dependsOn`, `usesTerm`) are ID
  references stored on the source node.
- `node.rs` — node types. Each node carries a `kind` tag; because serde ignores a
  struct-level tag when deserializing, nodes use `#[serde(remote = "Self")]` plus
  the `tagged!` macro so the tag is actually checked. Follow that pattern for new
  node types.
- `id.rs` — typed IDs (`Id<kind::Requirement>` etc., e.g. `REQ-042`) with strict
  canonical spelling; `DependencyId` is a cross-kind reference.
- `text.rs` — validated newtypes (`Title`, `Markdown`, `SpecName`) via `TryFrom<String>`.
- `optional.rs` — canonical form omits empty optional fields; pair these
  deserializers with `#[serde(default)]` so a present field must be non-empty.
- `check.rs` — invariants the schema can't express. **Deserializing a `SlcrRequirementsDocument`
  runs `check::invariants` and fails on any `Violation`**; non-fatal findings are
  `Warning`s, exposed separately via `SlcrRequirementsDocument::warnings()`. All violations are
  collected and reported together, in document order. To see the findings
  instead of failing, read an `UncheckedDocument` (or load
  `RequirementsFile::unchecked()`), whose `report()` returns a `check::Report`
  of every violation and warning.
- `file.rs` — `RequirementsFile`, a caller-chosen path whose extension selects the
  format.

Structs use `rename_all = "camelCase"` and `deny_unknown_fields`, and the canonical
fixture must round-trip byte-for-byte (`the_canon_example_serializes_back_to_itself`),
so field order and omission rules matter.

## Filesystem (`src/fs/`)

`FileSystem` wraps XDG project dirs and does all serde I/O, dispatching on
`File::extension()` to TOML / JSON / YAML (`serde-saphyr`). A file that can't be
read fails with a `ReadError` naming its path; one that can't be deserialized
fails with a `ParseError` that labels the problem in the file's source when the
deserializer reports a location. Implement `File` for
files at dynamic paths, or `StaticFile` for fixed-name files under a
`DirectoryType` (whose parent dir is created on demand).
