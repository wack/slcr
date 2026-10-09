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
cargo insta review             # Review snapshot changes after a failing run (see Testing)
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
(`src/lib.rs`) and a thin binary (`src/bin/main.rs`). Unit tests are inline
`#[cfg(test)]` modules; the only integration test is the CLI's end-to-end
snapshot suite in `tests/cli/` (see Testing). `tests/fixtures/` holds data only
(the canonical `todo-api` spec in all three formats and its rendering, loaded
via `include_str!`).

## Command flow

`main` parses `config::Cli` (clap derive), builds a `Terminal`, installs the miette
error hook, then calls `SlcrCommand::dispatch` (`src/config/command.rs`), which
constructs a per-command struct from `src/cli/` and calls its synchronous
`dispatch(self) -> miette::Result<()>`. To add a command: add a variant to
`SlcrCommand`, a struct in `src/cli/` that holds the `Terminal`, and a match arm.

- `slice` and `eval` are stubs returning `cli::NotImplemented`.
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
- `render [FILE]` renders a specification (default `SPEC.slcr.yml`) as
  Markdown with `render::document`, to FILE's base name plus `.md` beside it
  (`SPEC.slcr.yml` → `SPEC.md`), to `--output PATH`, or to stdout with `-o -`.
  Findings go to stderr as `check` reports them, and a file that breaks an
  invariant fails with `RenderFailed`. It replaces the output atomically
  (`FileSystem::replace_file`), but only a file that starts with the
  generated-file comment, unless `--force`; an up-to-date output is left
  alone. `--expect-no-diff` writes nothing and fails with `UnexpectedDiff`
  unless the output is already up to date. (The flag avoids the word "check",
  which is overloaded.)
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
- `tagged.rs` — how `tagged!` (and `SectionChild`) check the tag: the `kind` is
  read first, then the rest of the node's map streams to its derived
  deserializer, so a parse error is labeled where it is. Never deserialize nodes
  through a serde tagged enum (`#[serde(tag = ...)]` on an enum): serde buffers
  the whole map to read the tag, which relocates every error inside it to the
  root. Only fields written before a non-canonical, late `kind` are buffered.
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

## Rendering (`src/render/`)

`render::document` is a pure function from a `SlcrRequirementsDocument` to
the canonical Markdown of section 8 of the design document; the same graph
always renders to the same bytes. The canon fixture's rendering is the golden
file `tests/fixtures/todo-api.md`; when output changes on purpose, update it
with `slcr render tests/fixtures/todo-api.spec.yaml --force`.

- `blocks.rs` — `Blocks` joins blocks with exactly one blank line, LF endings,
  and one trailing newline. Prose (`body`, `rationale`, `definition`) is
  emitted verbatim apart from that normalization; never indent or rewrap it.
- `inline.rs` — titles are plain text: `escape` them for Markdown, or
  `escape_html` inside the Glossary's `<dl>`. Tests check every ASCII
  punctuation character round-trips through `pulldown-cmark`.
- Each node is preceded by a hidden `<a id="ID"></a>`; IDs never appear in
  visible text, and every cross-reference is a link to an anchor.

## Filesystem (`src/fs/`)

`FileSystem` wraps XDG project dirs and does all serde I/O, dispatching on
`File::extension()` to TOML / JSON / YAML (`serde-saphyr`). A file that can't be
read fails with a `ReadError` naming its path; one that can't be deserialized
fails with a `ParseError` that labels the problem in the file's source when the
deserializer reports a location. Generated text goes through
`replace_file`, which writes atomically via a temporary file and never
creates a directory. Implement `File` for
files at dynamic paths, or `StaticFile` for fixed-name files under a
`DirectoryType` (whose parent dir is created on demand).

# Testing

Two layers, both run by `cargo make test`:

- **Unit tests**, inline in `#[cfg(test)]` modules, test the library directly:
  exact strings, error types via `downcast_ref`, and `figment::Jail` for
  anything that touches the working directory.
- **CLI end-to-end snapshot tests**, in `tests/cli/`, run the real binary and
  snapshot what a user sees with [`insta`](https://insta.rs) and `insta-cmd`.
  They live outside `src/` because only integration tests get the built
  binary (`env!("CARGO_BIN_EXE_slcr")`).

## CLI snapshot tests (`tests/cli/`)

`tests/cli/main.rs` is the harness; each subcommand has its own module
(`init.rs`, `render.rs`, `check.rs`, `version.rs`), and `global.rs` covers
top-level behavior. Snapshots are in `tests/cli/snapshots/`, named
`cli__<module>__<name>.snap`, and are committed.

- **Isolation.** Every test makes a `Workspace` (a temp dir), writes its inputs
  with `workspace.file(..)`, and runs `workspace.slcr(&[..])` (or `slcr_in` for
  a subdirectory). Pass paths relative to the workspace so output is the same on
  every run. `slcr()` removes `LOG_LEVEL`, `CLICOLOR`, `CLICOLOR_FORCE`, and
  `NO_COLOR` so the developer's or CI's environment can't leak in; add to
  `AMBIENT` in `main.rs` if a new variable changes output. Don't `env_clear()`:
  coverage collection relies on inherited variables.
- **What to snapshot.** `assert_cmd_snapshot!(command)` records the exit code,
  stdout, and stderr. Assert side effects too: `workspace.files()` for what was
  written, `assert_eq!` against a fixture when one exists (e.g. rendered
  Markdown against `tests/fixtures/todo-api.md`), or `assert_snapshot!(name,
  contents)` for generated files that have no fixture (e.g. `init`'s output).
- **Naming.** A test with one snapshot takes its name from the test function. A
  test with several passes explicit, short names (`"refused"`, `"forced"`,
  `"json_file"`), never names derived from file names.
- **Coverage.** Each subcommand gets a `--help` snapshot, a snapshot for every
  argument and flag of its own, and one for each distinct error path, including
  clap usage errors (exit code 2). Global flags (`--log-level`, its `LOG_LEVEL`
  variable, `--enable-colors`) behave the same for every subcommand, so they're
  covered once, in `global.rs`; don't repeat them per subcommand. The stubs
  (`slice`, `eval`) have one snapshot each, so implementing them shows up as a
  snapshot change.
- **Nondeterminism.** Redact anything that changes between runs with an
  `insta::Settings` filter scoped to the test (see `without_timestamps` in
  `global.rs` and the `[VERSION]` redaction in `version.rs`). Never redact an
  absolute or temp path: one in output is a bug to fix in the code. The suite
  caught `render` leaking `tempfile`'s random temp-file path into an error this
  way.

### Workflow

- Install the reviewer once: `cargo install --locked cargo-insta`.
- When output changes, `cargo make test` fails and writes `*.snap.new` files
  beside the snapshots (they're git-ignored). Review them with `cargo insta
  review`, or inspect `cargo insta pending-snapshots` and run `cargo insta
  accept`. Never hand-edit a `.snap` file.
- A test that takes several snapshots stops at the first mismatch; run
  `INSTA_FORCE_PASS=1 cargo test --test cli` to collect every pending snapshot in
  one pass, then review.
- After renaming or deleting a test, run `cargo insta test
  --unreferenced=reject` to fail on orphaned snapshots, then delete them.
- In CI, insta sees the `CI` variable: it never writes snapshots, and any
  mismatch or missing snapshot fails the run. A snapshot must be committed
  before CI can pass.
