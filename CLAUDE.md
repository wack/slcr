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
(the canonical `todo-api` spec in all three formats, its rendering, and the
slice of its controller, loaded via `include_str!`).

## Command flow

`main` parses `config::Cli` (clap derive), builds a `Terminal`, installs the miette
error hook, then calls `SlcrCommand::dispatch` (`src/config/command.rs`), which
constructs a per-command struct from `src/cli/` and calls its synchronous
`dispatch(self) -> miette::Result<()>`. To add a command: add a variant to
`SlcrCommand`, a struct in `src/cli/` that holds the `Terminal`, and a match arm.

- `check` is a stub returning `cli::NotImplemented`. It will run every
  validator and report whether each requirement is satisfied, unsatisfied, or
  unknown; it vets the project first, and it is the only command that may
  fetch a descriptor missing from the store. "Check" means only this.
- `init [FILE]` creates a new specification (`SlcrRequirementsDocument::new`:
  root `SEC-001`, Glossary `SEC-002`) at `FILE`, by default the well-known
  `SPEC.slcr.toml` (`spec::file::DEFAULT_FILE`, also the default FILE of
  `render`, `query`, and `slice`). Without `--name`, the spec is named after the file's stem
  minus any `.slcr`/`.spec`, or, for `SPEC.slcr.*`, after its directory; an
  invalid result is an error, never slugified. `--title` defaults to the name.
  Unless `Project::find_from` finds an `SLCR.toml` in the working directory
  or a parent (which it leaves untouched, and says so), it also creates
  `SLCR.toml` in the working directory, with `formatVersion` and `specs`
  listing FILE relative to it; FILE outside the working directory, or with
  glob metacharacters, is then an error (`InvalidSpecPath`), as is a
  misnamed manifest. Both files go through `FileSystem::create_file`, so it
  never overwrites anything (even a dangling symlink) and never creates a
  missing directory; it creates both or neither, removing the spec if the
  manifest can't be written.
- `vet [FILE]...` loads each requirements file with
  `RequirementsFile::unvetted()`, writes every finding to stderr as a miette
  diagnostic (`Terminal::write_diagnostic`) and each file's status to stdout,
  and fails with `VetFailed` if any file can't be loaded or breaks an
  invariant (or has warnings, under `--deny-warnings`). `--format json` writes
  one JSON document to stdout instead (`src/cli/vet/json.rs`); findings
  serialize with a kebab-case `code` plus their fields, so renaming a
  `Violation` or `Warning` variant or field changes that output. Like
  `terraform validate`, `vet` runs nothing and never uses the network.
  Given no FILE, it vets the project (`src/project/`): it finds `SLCR.toml`
  as `tools` does, discovers the files its `specs` match, and vets each the
  same way, named relative to the working directory. The manifest counts as
  one more file, with its own status line (`SLCR.toml: 2 problems`): it fails
  if it can't be parsed or the project has a `project::Problem`, each written
  as its own diagnostic. In JSON, those go in a `project` object that only
  project mode adds, and problems serialize like findings, so renaming a
  `Problem` variant or field changes that output too. Vetting validator files
  and pinned descriptors is still to come.
- `render [FILE]` renders a specification (default `SPEC.slcr.toml`) as
  Markdown with `render::document`, to FILE's base name plus `.md` beside it
  (`SPEC.slcr.toml` → `SPEC.md`), to `--output PATH`, or to stdout with `-o -`.
  Findings go to stderr as `vet` reports them, and a file that breaks an
  invariant fails with `RenderFailed`. It replaces the output atomically
  (`FileSystem::replace_file`), but only a file that starts with the
  generated-file comment, unless `--force`; an up-to-date output is left
  alone. `--expect-no-diff` writes nothing and fails with `UnexpectedDiff`
  unless the output is already up to date. (The flag avoids the word "check",
  which means running validators.)
- `query QUERY [FILE]` runs a read-only GQL query (the SLCR query profile of
  `slcr-gql`) against a specification (default `SPEC.slcr.toml`) and writes the
  rows to stdout as an aligned table, or with `--format tsv` / `json`. The
  query is parsed before the file is read; `--param NAME=JSON` binds `$NAME`.
  Findings go to stderr as `vet` reports them, and a file that breaks an
  invariant fails with `QueryFailed`. Nodes are written as their SLCR IDs and
  edges as `(REQ-004)-[:REFINES]->(REQ-002)` (in JSON, an object with `label`,
  `source`, and `target`). A query that can't run fails with `InvalidQuery`,
  which labels syntax errors and features outside the profile in the query.
- `slice REQUIREMENT [FILE]` writes the slice of one requirement (default file
  `SPEC.slcr.toml`) to stdout as Markdown: `slice::members` finds what it
  includes with GQL, and `render::slice` renders it. Findings go to stderr as
  `vet` reports them; a file that breaks an invariant fails with
  `SliceFailed`, and an ID the file lacks with `UnknownRequirement`. A
  malformed ID is a clap usage error.
- `tools add SOURCE [--as NAME]` declares a tool in the project manifest,
  `SLCR.toml` (`src/tools/`): it finds the project root by walking up to
  the nearest `SLCR.toml`, fetches the descriptor from an `https://` URL or
  a path inside the project, validates it, pins its BLAKE3 hash, installs
  it in the descriptor store, and only then writes `[tools.<NAME>]`
  (`toml_edit`, so the rest of the file is untouched). The name defaults to
  the descriptor's `name`; a different tool already declared under it is an
  error, and re-adding an identical entry changes nothing.
- `tools update NAME` re-fetches the descriptor of the tool declared under
  NAME from its recorded source, validates and installs it, and re-pins the
  entry, editing only its `version` and `pin` values in place. A failed
  fetch or an invalid descriptor changes nothing, and an entry that already
  pins what its source serves is left alone.
- `tools remove NAME` deletes the entry, and the `tools` table with it if
  it was the last. The descriptor stays in the store, which other projects
  may pin it from.
- Commands stay synchronous; an async command calls `cli::block_on` from its
  `dispatch()` (see `src/cli/runtime.rs`), as `query` and `slice` do.
- All user-facing output goes through `Terminal` (`src/terminal/`), which owns
  color detection (`--enable-colors`) and logging setup (`--log-level` / `LOG_LEVEL`).
- Errors are `thiserror` + `miette::Diagnostic` types; use `#[diagnostic(help(...))]`
  for remediation hints rather than ad-hoc messages.

## Specification graph (`src/spec/`)

The data model mirrors the specification graph JSON Schema (draft 2020-12) kept
on the "SLCR Schemas" page in Notion — that page, not this repo, is the source of
truth for field names, grammar, and the `x-vetInvariants` list.

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
- `vet.rs` — invariants the schema can't express. **Deserializing a `SlcrRequirementsDocument`
  runs `vet::invariants` and fails on any `Violation`**; non-fatal findings are
  `Warning`s, exposed separately via `SlcrRequirementsDocument::warnings()`. All violations are
  collected and reported together, in document order. To see the findings
  instead of failing, read an `UnvettedDocument` (or load
  `RequirementsFile::unvetted()`), whose `report()` returns a `vet::Report`
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
- `slice.rs` — `render::slice` renders a slice with the same `Renderer`:
  R's ancestors as headings (bodies only), then R and the included
  requirements nested under it, then everything else under "Related
  requirements" with a breadcrumb, then a Glossary of the included terms.
  Nodes keep their document numbers. A requirement's parts are gated by its
  `slice::Detail`; a reference to a node the slice leaves out is its plain
  title, so every link resolves. The canon controller's slice is the golden
  file `tests/fixtures/todo-api.REQ-005.md`; update it with
  `slcr slice REQ-005 tests/fixtures/todo-api.spec.yaml > tests/fixtures/todo-api.REQ-005.md`.

## Slices (`src/slice.rs`)

`slice::members` computes `slice(R)` from section 9 of the design document
by running one-hop GQL queries against a `SpecGraph` until each closure stops
growing. Don't replace them with quantified patterns (`-[:REFINES]->{1,}`):
`slcr-gql` matches those once per path, which is exponential on a DAG with
diamonds. Each included node gets a `Detail` (`Body` for ancestors and what R
refines, `Dependency` for the `dependsOn` closure of R and its refinement
descendants, `Full` for R and those descendants); a node included for several
reasons gets the greatest.

## Tools (`src/tools/`)

The project manifest and tool descriptors, from the manifest and descriptor
JSON Schemas on the Notion "Schemas" page and the rules on "SLCR — Tools:
Manifest, Descriptors, and Invocation" (which win over the older
low-level design page). The MVP has exit-code results only, no lockfile,
and no caching.

- `add.rs`, `update.rs`, `remove.rs` — the whole of `slcr tools add`,
  `update`, and `remove`; `src/cli/tools.rs` only builds their
  `Environment` (`mod.rs`: filesystem, `DescriptorStore`, `Fetcher`, working
  directory) and prints the result. Test new behavior there, with the
  shared test `Fixture` (`fixture.rs`), and keep CLI snapshots for what a
  user sees.
- `manifest.rs` — `Project` (root discovery; a case-only near miss such as
  `slcr.toml` is an error, so macOS and Linux CI agree), `Manifest` (typed,
  `deny_unknown_fields`), `ManifestFile`, which edits the TOML document
  in place (`set_tool`, `repin_tool`, `remove_tool`), whether `tools` is
  written as `[tools.*]` headers or as an inline table, and `NewManifest`,
  what `slcr init` writes.
- `descriptor.rs` — `Descriptor::parse` checks the schema's grammar through
  serde, then the `x-vetInvariants` that concern the descriptor alone:
  `validator.schema` compiles as a self-contained draft 2020-12 schema of
  `type = "object"`, and every `{with.<field>}` names a required scalar
  property. Argument templates are parsed into `Segment`s up front.
- `pin.rs` — `blake3:` plus 64 lowercase hex; nothing else parses.
- `source.rs` — `{ url }` or `{ path }` (project-relative, and still inside
  the project after following symlinks). `Fetcher` fetches URLs with async
  `reqwest`: HTTPS only, even across redirects, bodies capped at 1 MiB. It
  builds its client on first use, so path sources never load the platform's
  certificate authorities.
- `https_server.rs` (tests only) — a local HTTPS server with a throwaway
  certificate authority, and a production `Fetcher` that trusts only it, so
  fetching is tested for real: redirects, statuses, the size cap.
- `store.rs` — the content-addressed store at
  `<data dir>/descriptors/blake3/<hex>.toml`; `install` takes a descriptor's
  text and returns the pin it filed it under. The data directory is the
  `directories` crate's `ProjectDirs::data_dir()`: `$XDG_DATA_HOME/slcr` on
  Linux (`~/.local/share/slcr` if unset), `~/Library/Application
  Support/slcr` on macOS.

## Projects (`src/project/`)

A project's specifications: the files the manifest's `specs` match, loaded
and indexed by `spec.name`. The rules are the manifest schema's and those on
"SLCR — Tools: Manifest, Descriptors, and Invocation". Every problem is
collected, never just the first, as a `Problem` (`problem.rs`), whose paths,
like `SpecFile::shown`, are relative to the working directory.

- `discover.rs` — `discover` expands each pattern under the project root:
  `globset` with `literal_separator`, so `*` never crosses a `/` but does
  match a leading dot, and an `ignore` walk that sees hidden files (never
  `.git`) and skips what git ignores: in a git repository, the `.gitignore`
  files of the project and its parents up to the repository's root,
  `.git/info/exclude`, and the global excludes; outside one, as for git,
  nothing (`require_git(true)`). A
  file matched twice counts once; files are ordered by project-relative
  path. A pattern that is invalid or matches nothing, a match that is a
  validator file (`.validators.slcr.*`) or has no specification format's
  extension, and an unreadable directory are each a `Problem`.
- `mod.rs` — `SpecFile`: a discovered file's project-relative, shown, and
  absolute paths; `load` reads it unvetted through
  `FileSystem::load_file_at`, so errors name the shown path.
- `index.rs` — `SpecIndex` borrows loaded specifications by name; a second
  file with a name already taken is a `DuplicateSpecName` problem naming
  both. `resolve` turns a `QualifiedRequirementId` into its specification
  and requirement, or fails with `UnresolvedRequirement` (`UnknownSpec`,
  whose help lists the names, or `UnknownRequirement`).
- `qualified.rs` — `QualifiedRequirementId`, such as `todo-api/REQ-002`: a
  `SpecName`, `/`, and a `RequirementId`, the validator schema's pattern.
  The qualifier is the name, never a path.

## Queries (`src/query/`)

`slcr-gql` (a git dependency on `wack/gql-rust`, pinned by `rev`) parses and
runs queries against any `GraphStore`. Its `PROFILE.md` specifies the query
language and the graph a specification maps to.

- `materialize.rs` — loads a vetted `SlcrRequirementsDocument` into a
  `GraphStoreMut`, following that mapping. `slcr_gql::load_spec` implements
  the same mapping from JSON alone; the tests hold the two to the same graph,
  element for element and **in the same insertion order** (rows without
  `ORDER BY` come back in insertion order). Change both together, or the tests
  fail.
- `mod.rs` — `SpecGraph`: a document materialized into an in-memory
  `PetgraphStore`, which runs parsed queries and resolves results into
  `Cell`s (`cell.rs`) that name nodes by SLCR ID.
- `error.rs` — `InvalidQuery`, the miette diagnostic for any query error.

## Filesystem (`src/fs/`)

`FileSystem` wraps XDG project dirs and does all serde I/O, dispatching on
`File::extension()` to TOML / JSON / YAML (`serde-saphyr`). A file that can't be
read fails with a `ReadError` naming its path; one that can't be deserialized
fails with a `ParseError` that labels the problem in the file's source when the
deserializer reports a location; `load_file_at` reads a file from one path,
such as an absolute one, while errors name it by the `File`'s own path.
Generated text goes through
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
(`init.rs`, `render.rs`, `vet.rs`, `query.rs`, `slice.rs`, `tools.rs`, `version.rs`), and
`global.rs` covers top-level behavior. Snapshots are in `tests/cli/snapshots/`, named
`cli__<module>__<name>.snap`, and are committed.

- **Isolation.** Every test makes a `Workspace` (a temp dir), writes its inputs
  with `workspace.file(..)`, and runs `workspace.slcr(&[..])` (or `slcr_in` for
  a subdirectory). Pass paths relative to the workspace so output is the same on
  every run. `slcr()` removes `LOG_LEVEL`, `CLICOLOR`, `CLICOLOR_FORCE`, and
  `NO_COLOR` so the developer's or CI's environment can't leak in; add to
  `AMBIENT` in `main.rs` if a new variable changes output. It also points
  `XDG_DATA_HOME` (read on Linux) and `HOME` (read on macOS) into the
  workspace, so `slcr`'s data directory, and with it the descriptor store,
  is the workspace's `DATA_DIR`, never the real one. insta-cmd records every
  variable a command sets in the snapshot's header, so `main.rs` wraps
  `assert_cmd_snapshot!` to redact those two (headers only, never output);
  use the wrapper, not `insta_cmd`'s. Don't `env_clear()`:
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
  covered once, in `global.rs`; don't repeat them per subcommand. The stub
  (`check`) has one snapshot, so implementing it shows up as a snapshot
  change.
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
