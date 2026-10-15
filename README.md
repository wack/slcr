# SLCR

An open standard for agent-native software requirements.

SLCR stores a project's requirements as a graph instead of a document. Agents
read a slice of the graph: one requirement plus the context it needs to be
implemented, and nothing else. Humans read Markdown rendered from the same
graph. Validators that live beside your code check each requirement, so
"does the code do what the spec says?" becomes a command you can run in CI.

This repository holds `slcr`, the reference CLI, along with a Claude skill
that teaches coding agents to use it.

## Contents

- [Why SLCR](#why-slcr)
- [How it works](#how-it-works)
- [Install](#install)
- [Quick start](#quick-start)
- [Commands](#commands)
  - [Querying a spec](#querying-a-spec)
  - [Reading `check` results](#reading-check-results)
- [In CI](#in-ci)
- [Teach your agent SLCR](#teach-your-agent-slcr)
- [SLCR specifies itself](#slcr-specifies-itself)
- [Status](#status)
- [Contributing](#contributing)
- [License](#license)

## Why SLCR

Coding agents work from specs now, and a spec is only useful if the code keeps
matching it. Two things get in the way on long-running work:

- **Context rot.** Hand an agent a long requirements document and it will
  conflate details or drop instructions. The more prose up front, the worse
  it gets.
- **Compounding drift.** A small chance of misreading the spec on each ticket
  adds up over a multi-ticket project. By the end, something has usually
  drifted, and nobody has a cheap way to find out what.

SLCR addresses the first by giving agents slices instead of whole documents,
and the second by tying every requirement to validators that pass or fail.

## How it works

A **specification** is a tree of sections, requirements, acceptance criteria,
and glossary terms, written in YAML, JSON, or TOML. Each requirement has an
ID that never changes and a modality from
[RFC 2119](https://www.rfc-editor.org/rfc/rfc2119) (`MUST`, `SHOULD`, `MAY`).
Requirements can refine other requirements, combined with `AND`, `OR`, or
`XOR`, and can depend on requirements elsewhere in the spec.

A **slice** is what an agent reads to implement one requirement: the
requirement, its refinements, the sections it sits in, what it depends on, and
the glossary terms it uses.

A **validator** proves one acceptance criterion by running a tool, like a test
runner or `ast-grep`. Validators live in `.validators.slcr.toml` files beside
the code they check. The tools they run are declared and pinned in the project
manifest, `SLCR.toml`. There are no built-in tools.

`slcr check` runs every validator and reports each requirement as satisfied,
unsatisfied, or unknown. Unknown means no validator covers it yet.

## Install

With [Homebrew](https://brew.sh), on macOS or Linux:

```bash
brew install wack/tap/slcr
```

Or with the shell installer from the
[latest release](https://github.com/wack/slcr/releases/latest).

## Quick start

Create a specification and a project manifest:

```console
$ slcr init SPEC.slcr.yaml --name todo-api --title "TodoList API"
SPEC.slcr.yaml: created specification todo-api
SLCR.toml: created project manifest
```

Add requirements under the root section. Here `REQ-001` has an acceptance
criterion a validator can prove, and `REQ-002` doesn't have one yet:

```yaml
root:
  kind: section
  id: SEC-001
  title: TodoList API
  children:
    - kind: requirement
      id: REQ-001
      title: POST creates an item
      body: The endpoint MUST accept POST to create an item.
      modality: MUST
      acceptanceCriteria:
        - kind: acceptanceCriterion
          id: AC-001
          title: Returns the created item
          body: POST /items with a valid payload returns 201 and the new item.
    - kind: requirement
      id: REQ-002
      title: Rejects duplicate titles
      body: The endpoint SHOULD reject an item whose title already exists.
      modality: SHOULD
```

Check that the spec is well formed, then print the slice an agent would read:

```console
$ slcr vet
SLCR.toml: ok
SPEC.slcr.yaml: ok

$ slcr slice REQ-001 SPEC.slcr.yaml
```

(`slice`, `render`, and `query` read `SPEC.slcr.toml` unless you name a file.)

Declare the tool your validators run. A tool is described by a small TOML
descriptor that says how to invoke it. This one runs a single test with
[cargo-nextest](https://nexte.st):

```toml
# tools/cargo-nextest.toml
formatVersion = "1"
name = "cargo-nextest"
version = "1.0.0"

[validator.schema]
type = "object"
required = ["test"]
additionalProperties = false
properties.test = { type = "string", minLength = 1 }

[invoke]
argv = ["cargo", "nextest", "run", "--no-tests=fail", "-E", "test(={with.test})"]
cwd = { nearest = "Cargo.toml" }

[results]
pass = [0]
```

```console
$ slcr tools add tools/cargo-nextest.toml
SLCR.toml: added tool cargo-nextest 1.0.0
```

Write a validator beside the code it checks, pointing at the acceptance
criterion it proves:

```toml
# tests/.validators.slcr.toml
formatVersion = "1"

[[validator]]
requirement = "todo-api/REQ-001"
criterion = "AC-001"
tool = "cargo-nextest"
with = { test = "items::post_returns_the_created_item" }
```

Run the validators:

```console
$ slcr check
tests/.validators.slcr.toml validator 1 (todo-api/REQ-001 AC-001, cargo-nextest): green
todo-api: 1 satisfied, 0 unsatisfied, 1 unknown (1 coverage gap)
```

`REQ-002` shows up as a coverage gap until something validates it. Finally,
render the spec for people to read:

```console
$ slcr render SPEC.slcr.yaml
SPEC.md: rendered todo-api
```

## Commands

| Command | What it does |
| --- | --- |
| `slcr init` | Create a specification, and `SLCR.toml` if the project doesn't have one |
| `slcr vet` | Check that specs, validator files, and the manifest are well formed, without running anything |
| `slcr slice <REQ>` | Print the context an agent needs to implement one requirement |
| `slcr check` | Run every validator and report which requirements are satisfied |
| `slcr render` | Render a specification as Markdown, to `SPEC.md` by default |
| `slcr query <GQL>` | Run a read-only GQL query over a specification |
| `slcr tools` | Add, update, install, list, and remove the tools validators run |

Run `slcr <command> --help` for flags. `vet`, `check`, and `query` can emit
JSON for scripts and agents.

### Querying a spec

`slcr query` takes read-only GQL. Node labels are `Section`, `Requirement`,
`AcceptanceCriterion`, and `Term`. Edge labels are `CONTAINS`,
`HAS_CRITERION`, `REFINES`, `DEPENDS_ON`, and `USES_TERM`.

```console
$ slcr query "MATCH (r:Requirement) RETURN r.id, r.modality, r.title" SPEC.slcr.yaml
r.id     r.modality  r.title
-------  ----------  ------------------------
REQ-001  MUST        POST creates an item
REQ-002  SHOULD      Rejects duplicate titles
(2 rows)
```

To find what refines a requirement:

```bash
slcr query "MATCH (c:Requirement)-[:REFINES]->(p:Requirement {id: 'REQ-001'}) RETURN c.id, c.title"
```

### Reading `check` results

An unsatisfied `MUST` or `MUST NOT` requirement fails the run. An unsatisfied
`SHOULD` is a warning, and an unsatisfied `MAY` is informational. Pass
`--deny-warnings` to fail on warnings too. A failing validator means the code
or the validator is wrong, not that the requirement should be loosened.

## In CI

```bash
slcr vet --deny-warnings
slcr check
slcr render --expect-no-diff   # fails if SPEC.md is stale
```

## Teach your agent SLCR

[`skills/slcr`](skills/slcr) is a Claude skill that teaches coding agents why
and when to use SLCR, how to write specs and validators, and how to learn the
CLI from `--help`. See [`skills/README.md`](skills/README.md) to add it to
your repository.

A typical agent loop: read the slice for a requirement, write a validator that
fails, implement until it passes, then run `slcr check`.

## SLCR specifies itself

This repository is an SLCR project. [`SPEC.slcr.toml`](SPEC.slcr.toml) holds
the CLI's requirements, [`SPEC.md`](SPEC.md) is its rendering, and
[`tests/cli/.validators.slcr.toml`](tests/cli/.validators.slcr.toml) ties
each acceptance criterion to a snapshot test. `slcr check` in this repo checks
`slcr`.

## Status

SLCR is pre-release. The only build today is the rolling
[edge release](https://github.com/wack/slcr/releases/latest) from `trunk`, and
the formats may still change before 1.0. Validators currently report pass or
fail by exit code, and every `slcr check` reruns every validator. Structured
validator results and result caching are planned.

## Contributing

Issues and pull requests are welcome, especially feedback on the format.
To build and test locally:

```bash
cargo build --release
cargo make clippy
cargo make test
```

If you change the CLI's behavior, update `SPEC.slcr.toml`, run
`slcr render`, and make sure `slcr check` still passes.

## License

[MIT](LICENSE)
