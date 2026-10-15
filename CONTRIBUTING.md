# Contributing to SLCR

Thanks for helping out. SLCR is two things that live in this repository: an
open format for software requirements, and `slcr`, its reference CLI. Changes
to each go through a different process, so this guide covers them separately.

## Contents

- [Ways to contribute](#ways-to-contribute)
- [Reporting a bug](#reporting-a-bug)
- [Proposing a change to the format](#proposing-a-change-to-the-format)
- [Changing the CLI](#changing-the-cli)
  - [Set up](#set-up)
  - [Build and test](#build-and-test)
  - [Snapshot tests](#snapshot-tests)
  - [Keep the spec in step](#keep-the-spec-in-step)
- [Opening a pull request](#opening-a-pull-request)
- [License](#license)

## Ways to contribute

- **Use SLCR on a real project and tell us what broke.** Feedback on the
  format is the most useful thing you can give right now, before it settles.
- **Report bugs** in the CLI.
- **Propose format changes**, such as a new field or node kind.
- **Fix bugs or add features** to the CLI.
- **Improve the docs or the [agent skill](skills/slcr).**
- **Share a tool descriptor** that worked for you, for a test runner or
  linter other people might want.

## Reporting a bug

Open an issue that includes:

- The output of `slcr version`.
- The command you ran, and what it printed.
- What you expected instead.
- The smallest spec, validator file, or `SLCR.toml` that reproduces it.

For `vet`, `check`, and `query`, the `--format json` output is often the
fastest way to show us what went wrong.

## Proposing a change to the format

The format covers every file SLCR reads: specifications, validator files, the
`SLCR.toml` manifest, and tool descriptors. Other tools are meant to read and
write these files too, so a change here affects more than this CLI.

**Open an issue before writing code.** A format pull request without a
discussion first will likely be asked to start over as an issue. In the issue,
include:

- **The problem.** What can't you express today, or what is awkward to
  express? A real example helps more than an abstract one.
- **The proposal.** Show it in an actual spec or validator file, as you'd
  want to write it.
- **Effects on the CLI.** How it changes slicing, `check` results, or the
  rendered Markdown, if it does.
- **Compatibility.** Every SLCR file declares a `formatVersion`. Say whether
  existing files stay valid. A change that breaks them needs a new format
  version, and a much stronger case.

Once a proposal is accepted, the schema and the reference implementation
change together, in the same pull request or in pull requests that land
together. The CLI should never accept a file the schema rejects, or the
reverse.

## Changing the CLI

Small fixes can go straight to a pull request. For anything larger, or
anything that changes what the CLI prints, open an issue first so we can
agree on the behavior before you build it.

### Set up

You'll need [rustup](https://rustup.rs). The toolchain is pinned in
`rust-toolchain.toml`, so rustup installs the right version the first time
you build. Then install the cargo plugins the build uses:

```bash
cargo install --locked cargo-make cargo-nextest cargo-insta
```

Turn on the repository's git hooks once per clone:

```bash
cargo make install-hooks
```

The pre-commit hook refuses a commit whose `SPEC.md` doesn't match
`SPEC.slcr.toml`. More on that [below](#keep-the-spec-in-step).

### Build and test

Use the `cargo make` tasks rather than plain `cargo test` or `cargo clippy`.
They run the same `--locked` commands CI runs, so a clean local run means a
clean CI run.

```bash
cargo make check-format           # formatting
cargo make clippy                 # lints, with warnings as errors
cargo make test                   # unit and CLI tests, under nextest
cargo make self-check             # slcr check on this repo's own spec
cargo make render-expect-no-diff  # SPEC.md matches SPEC.slcr.toml
```

To run a subset of tests by name:

```bash
cargo nextest run --locked <filter>
```

### Snapshot tests

The CLI's end-to-end tests live in `tests/cli/`. They run the real binary
and snapshot its exit code, stdout, and stderr with
[insta](https://insta.rs). Snapshots are committed in
`tests/cli/snapshots/`.

When you change what the CLI prints, `cargo make test` fails and writes
`.snap.new` files next to the old snapshots. Review them with:

```bash
cargo insta review
```

Never hand-edit a `.snap` file. A new subcommand or flag needs snapshots for
its `--help`, for each argument and flag, and for each distinct error path.
Never redact an absolute or temporary path in a snapshot; if one shows up in
output, that's a bug to fix in the code.

### Keep the spec in step

This repository is an SLCR project. `SPEC.slcr.toml` holds the CLI's
requirements, and every CLI test proves at least one acceptance criterion
through `tests/cli/.validators.slcr.toml`. When you change CLI behavior,
change these together:

1. The requirement or acceptance criterion in `SPEC.slcr.toml`.
2. The test that proves it.
3. The validator that points the criterion at the test.

A few rules keep the spec stable:

- **IDs are never renumbered or reused.** Take the next free one:

  ```bash
  slcr query "MATCH (n) WHERE n.id STARTS WITH 'REQ-' RETURN n.id ORDER BY n.id DESC LIMIT 1"
  ```

- **Requirement bodies state their modality keyword** (`MUST`, `SHOULD`,
  and so on). CI denies warnings, so a missing keyword fails the build.
- **Renaming a test means updating its validator,** or `slcr check` fails.
- **Re-render after any spec change** and commit `SPEC.md` alongside it:

  ```bash
  cargo run -- render
  ```

The [agent skill](skills/slcr) covers writing requirements and validators in
more depth.

## Opening a pull request

- Keep each pull request to one change. Refactors and behavior changes are
  easier to review apart.
- Explain what changed and why, and link the issue if there is one.
- CI must pass. It runs formatting, clippy, the test suite, and the
  specification job, which runs `slcr check --deny-warnings` and
  `slcr render --expect-no-diff` on this repo.

Using a coding agent is fine. [`CLAUDE.md`](CLAUDE.md) describes the
architecture and conventions in detail, and agents that read it tend to
follow them. You're still responsible for reviewing what it wrote before
you open the pull request.

## License

SLCR is [MIT licensed](LICENSE). By contributing, you agree that your
contributions are licensed under the same terms.
