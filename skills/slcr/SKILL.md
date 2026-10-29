---
name: slcr
description: How to work in a repository that keeps its requirements in SLCR (Sliceable, Linked, Checkable Requirements) with the `slcr` CLI. Use it whenever the repository has an `SLCR.toml`, a `SPEC.slcr.*` or other `*.slcr.toml|yaml|json` spec, or `.validators.slcr.*` files; whenever someone mentions a requirement ID like `REQ-012` or `todo-api/REQ-002`, a slice, acceptance criteria, validators, or `slcr check`/`vet`/`render`/`slice`; and whenever you're asked to write, change, or review requirements or a spec, implement a feature the spec describes, prove a requirement is met, or fix a failing SLCR CI job. Use it even when the user doesn't say "SLCR" but the repo uses it, because the spec, not the chat, says what "done" means.
---

# Working with SLCR

SLCR stores a project's requirements as a **graph** in a YAML, JSON, or TOML
file, not as a document. That is what lets `slcr` give you a **slice** of one
requirement: the requirement, the background its ancestors provide, what it
refines into, what it depends on, and the terms it uses, and nothing else.
**Validators**, kept in files beside the code, prove requirements against the
code, and `slcr check` runs them and reports each requirement as satisfied,
unsatisfied, or unknown.

## Why this matters to you

- **The spec says what "done" means.** When a task touches behavior the spec
  describes, the requirement's slice is the right context: read the slice
  before you write the code, so you build what was specified.
- **A slice holds less, on purpose.** It leaves out sibling requirements that
  aren't yours to implement. Reading the whole spec invites scope creep.
- **`slcr check` is how you prove the work.** A requirement is done when its
  validators pass, not when the code looks right. An *unknown* requirement has
  no validator yet: that's a coverage gap to close, not a pass.
- **The spec is yours to edit.** In SLCR, agents edit the spec file directly
  and humans read the rendered Markdown. Rendered Markdown (`SPEC.md`, slices)
  is generated output, so never edit it by hand: change the spec and
  re-render.

## Learn the CLI from `--help`, one level at a time

This skill explains concepts. The installed `slcr` says which commands and
flags exist, so ask it rather than relying on memory or on this file, which
may be older or newer than the binary.

1. `slcr --help` lists the subcommands, one line each.
2. `slcr <command> --help` gives that command's arguments, flags, defaults,
   and output formats. Read it before you use a command for the first time in
   a session.
3. Commands with subcommands go one level deeper: `slcr tools --help`, then
   `slcr tools show --help`.

Use `-h` for the short form. Don't guess flags: a typo is a usage error
(exit status 2) that prints the usage, and that usage is a hint to run
`--help`. Don't try to fake its output.

For what `--help` doesn't cover, such as concepts, guides, and the schemas,
read the documentation at <https://slcr.dev/docs>.

## Install `slcr` if it's missing

If `command -v slcr` finds nothing, install it. With
[Homebrew](https://brew.sh), on macOS or Linux:

```bash
brew install wack/tap/slcr
```

Otherwise, on macOS or Linux, use the shell installer from the latest release.
It puts `slcr` in `$CARGO_HOME/bin` (by default `~/.cargo/bin`), so add that
to `PATH` if it isn't there:

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/wack/slcr/releases/latest/download/slcr-installer.sh | sh
```

In a disposable sandbox, such as a cloud or CI session, install it yourself.
On someone's own machine, ask before you install anything. Then run
`slcr --version` to confirm it works.

Here's an overview so you know what to ask about. Confirm the details with
`--help`.

| Command | What it's for |
|---|---|
| `slcr init [FILE]` | Create a new spec (root Section `SEC-001` and Glossary `SEC-002`), plus `SLCR.toml` if no project exists yet. |
| `slcr slice REQ-ID [FILE]` | Print one requirement's slice as Markdown: your context for implementing it. |
| `slcr vet [FILE]...` | Report structural and logical errors in specs, validator files, and the manifest. Runs nothing and never uses the network. Without FILE, vets the whole project. |
| `slcr check` | Run every validator and report whether each requirement is satisfied. Vets the project first. |
| `slcr render [FILE]` | Regenerate the Markdown rendering (by default `SPEC.slcr.toml` → `SPEC.md`). |
| `slcr query QUERY [FILE]` | Run a read-only GQL query against a spec: find IDs, edges, and gaps. |
| `slcr tools ...` | Declare, pin, install, and inspect the external tools that validators run. |

Two words have fixed meanings here. **vet** means "is it well-formed?" (like
`terraform validate`), and **check** means "do the validators pass?". Don't
use one to mean the other.

`slcr` finds the project by walking up from the working directory to the
nearest `SLCR.toml`. Commands that take a FILE default to `SPEC.slcr.toml`, so
in a project whose specs live elsewhere, pass the file. `SLCR.toml`'s `specs`
globs list every spec.

## Workflows

### Orient yourself in an SLCR repository

1. Read `SLCR.toml`: it lists the specs (`specs`) and the tools validators may
   use (`[tools]`).
2. Run `slcr vet` to see whether the project is healthy before you change
   anything, so you know which problems you started with.
3. Find what you need with `slcr query` rather than by scrolling. A few
   examples:
   ```bash
   slcr query "MATCH (r:Requirement) RETURN r.id, r.title ORDER BY r.id"
   slcr query "MATCH (c:Requirement)-[:REFINES]->(p:Requirement {id: 'REQ-001'}) RETURN c.id, c.title"
   slcr query "MATCH (n) WHERE n.id STARTS WITH 'REQ-' RETURN n.id ORDER BY n.id DESC LIMIT 1"
   ```
   Node labels are `Section`, `Requirement`, `AcceptanceCriterion`, and
   `Term`. Edge labels are `CONTAINS`, `HAS_CRITERION`, `REFINES`,
   `DEPENDS_ON`, and `USES_TERM`. Nodes have `id`, `title`, and their spec
   fields (`body`, `modality`, `refinement`, ...).

### Implement a requirement

1. `slcr slice REQ-ID [FILE]` and read all of it: ancestors' background, the
   requirement, its refinements and acceptance criteria, "Related
   requirements" (its dependencies), and the Glossary.
2. Implement what the slice asks for. If the slice is ambiguous or seems
   wrong, raise it, or fix the spec when the user wants that. Don't silently
   build something else.
3. Prove it with validators (see below), ideally red first: write the
   validator, watch it fail, implement until it passes.
4. Run `slcr check`. It should report the requirement satisfied.

### Write or change the spec

Read [references/writing-specs.md](references/writing-specs.md) before your
first edit to a spec file. It covers the file format, node kinds, IDs, the
four edges, AND/OR/XOR refinement, modality, acceptance criteria, and the
`vet` errors you're most likely to hit. In brief:

- Each requirement is one testable statement whose `body` states its RFC 2119
  keyword (`MUST`, `MUST NOT`, `SHOULD`, `SHOULD NOT`, `MAY`), matching its
  `modality` field.
- Nesting is placement only. A child counts toward its parent only if it lists
  the parent in `refines`, and a parent that something refines needs a
  `refinement` (`AND`, `OR`, or `XOR`).
- Take the next free ID of each kind (`REQ-`, `SEC-`, `AC-`, `TERM-`). Never
  renumber an ID or reuse a deleted one, because validators elsewhere in the
  repository refer to them.

After every edit:

1. `slcr vet` (or `slcr vet FILE`), and fix every finding. Use
   `--deny-warnings` if the project's CI does.
2. `slcr render` if the repository commits the rendered Markdown. CI often runs
   `slcr render --expect-no-diff`, which fails when the committed rendering is
   stale.
3. When behavior changes, update the requirement, its acceptance criteria,
   the code, and its validators together, so `slcr check` keeps proving what
   the spec says.

### Prove requirements with validators

Read [references/validators.md](references/validators.md) for validator files,
the manifest's `[tools]`, descriptors, and how `check` decides satisfaction.
In brief:

- A validator is an entry in the `.validators.slcr.toml` (or `.yaml`, `.yml`,
  `.json`) file in the directory of the code it checks. It names a qualified
  requirement (`<spec-name>/REQ-NNN`), optionally the `criterion` (`AC-NNN`)
  it proves, a `tool` declared in `SLCR.toml`, and the tool's fields under
  `with`.
- `slcr tools list` shows the declared tools, and `slcr tools show NAME`
  shows the schema `with` must satisfy. Never write a `pin` by hand: use
  `slcr tools add`, `update`, or `install`.
- `slcr vet` checks validator files without running them, and `slcr check`
  runs them.

### Read `slcr check` results honestly

- **red** (a validator failed): the code doesn't meet the requirement, or the
  validator is wrong. Read the output `check` prints under it, and fix the
  code or the validator. Don't loosen the requirement just to get green; if
  you think the requirement itself is wrong, say so.
- **Spawn failure** (the tool couldn't start: not on PATH, a bad descriptor,
  an unresolvable working directory): this is an environment problem, not a
  code problem, so don't change code to fix it.
- **Unknown**: the requirement has no validator. It never fails `check`, but
  report the gap and add a validator when the task is to prove that
  requirement.
- Severity follows modality: an unsatisfied MUST or MUST NOT is a violation
  and fails `check`. SHOULD and SHOULD NOT failures are warnings, which fail
  only under `--deny-warnings`. MAY failures are informational.
