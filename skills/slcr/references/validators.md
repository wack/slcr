# Validators, tools, and `slcr check`

Validators connect requirements to code. A validator is an entry in a file
beside the code it checks. It says "this requirement holds if this tool,
run like this, succeeds." `slcr check` runs every validator and evaluates
every spec from the results.

The dependency runs one way: validators name requirements, and requirements
never name validators. That keeps implementation detail out of the spec that
stakeholders read.

## Contents

1. [The project manifest, `SLCR.toml`](#the-project-manifest-slcrtoml)
2. [Tools and descriptors](#tools-and-descriptors)
3. [Validator files](#validator-files)
4. [Writing a validator](#writing-a-validator)
5. [What `slcr check` does](#what-slcr-check-does)
6. [Fresh clones, CI, and sandboxes](#fresh-clones-ci-and-sandboxes)

## The project manifest, `SLCR.toml`

The directory that holds `SLCR.toml` is the project root. `slcr` finds it by
walking up from the working directory. The file name is case-sensitive, so
`slcr.toml` is an error.

```toml
formatVersion = "1"
specs = ["SPEC.slcr.toml", "docs/**/*.slcr.yaml"]

[tools.cargo-nextest]
version = "1.0.0"
source = { url = "https://example.com/descriptors/cargo-nextest/1.0.0.toml" }
pin = "blake3:9f2c…"
```

- `specs` are globs relative to the root. A pattern that matches nothing, or
  that matches a validator file, is an error. Spec names must be unique
  across the project.
- `[tools.<name>]` entries are written by `slcr tools add`, `update`, and
  `remove`, which edit the file in place. **Never write or edit a `pin` by
  hand.** It's a BLAKE3 hash of the descriptor's bytes, and `slcr` refuses to
  run a descriptor that doesn't match it.

## Tools and descriptors

SLCR ships no tools. Each project declares the external programs its
validators run (a test runner, an AST matcher, a script). A **descriptor** is
a small, declarative TOML file that tells `slcr` how to run one tool:

```toml
formatVersion = "1"
name = "cargo-nextest"
version = "1.0.0"                      # the descriptor's version, not the tool's

[validator.schema]                     # JSON Schema that each validator's `with` must satisfy
type = "object"
required = ["test"]
additionalProperties = false
properties.test = { type = "string", minLength = 1 }

[invoke]
argv = ["cargo", "nextest", "run", "--no-tests=fail", "-E", "test(={with.test})"]
cwd = { nearest = "Cargo.toml" }       # or "project", or "validator-dir"
# env = { ... }   timeout = 600        # both optional

[results]
pass = [0]                             # exit codes that mean green
```

- `argv[0]` is a bare program name, looked up on PATH. Each element is
  exactly one argument, with no shell. `{with.<field>}` substitutes a scalar
  from the validator's `with`, and `{slcr.project_root}`,
  `{slcr.validator_dir}`, and `{slcr.requirement}` are built-ins.
- Stdin is closed. An exit code in `pass` is green. Any other exit code, a
  signal, or a timeout is red.

Useful commands (check each with `--help` for its options):

- `slcr tools list`: declared tools and whether each descriptor is installed.
- `slcr tools show NAME`: how a tool runs and the schema its `with` must
  satisfy. **Read this before writing a validator for a tool.**
- `slcr tools add SOURCE [--as NAME]`: declare a tool from an `https://` URL
  or a path inside the project.
- `slcr tools update NAME`: re-fetch and re-pin, for example after editing a
  descriptor kept in the repository. An edit takes effect only once
  re-pinned.
- `slcr tools install`: install what the manifest pins, never re-pinning.
- `slcr tools remove NAME`.

If no declared tool can express a check, a descriptor that runs a small script
committed to the repository is a legitimate tool. Ask before adding tools to
the project.

## Validator files

- The name is fixed: `.validators.slcr.toml`, `.yaml`, `.yml`, or `.json`.
  There is at most one per directory, and it sits beside the code it checks,
  so it moves with that code.
- `slcr` discovers validator files by walking the project root. It sees
  hidden files and skips git-ignored paths.
- `slcr vet` checks them against the specs, the manifest, and each tool's
  schema without running anything.

```toml
# src/views/items/.validators.slcr.toml
formatVersion = "1"

[[validator]]
requirement = "todo-api/REQ-002"
criterion = "AC-004"
tool = "ast-grep"
description = "POST handler is defined and registered with the router"
with = { rule = "rules/post-handler.yml" }

[[validator]]
requirement = "todo-api/REQ-002"
tool = "cargo-nextest"
with = { test = "items::post::creates_item" }
```

| Field | Meaning |
|---|---|
| `requirement` | Required. Always qualified, as `<spec-name>/REQ-NNN`, even in a single-spec project. The qualifier is the spec's `name`, not its path. |
| `tool` | Required. A key under `[tools]` in `SLCR.toml`. |
| `criterion` | Optional. The `AC-NNN` this validator proves. It must belong to that same requirement. |
| `description` | Optional free text shown in reports. Useful in JSON, which has no comments. |
| `with` | The tool's fields, validated against its descriptor's `validator.schema`. Omit it only if that schema requires nothing. |

Validators are anonymous: reports name them by file and position
(`src/.validators.slcr.toml validator 2`). Two identical entries in one file
are an error.

## Writing a validator

1. Pick the requirement, and ideally the acceptance criterion, you are
   proving. Read its slice first.
2. Pick the tool with `slcr tools list`, and read `slcr tools show NAME` for
   the exact `with` fields.
3. Add the entry to the validator file in the directory of the code it checks,
   creating the file if there isn't one. Keep entries grouped and commented
   the way the file already does.
4. Write the check (the test, the rule) so that it can fail. A validator that
   always passes proves nothing. Prefer red-green: see it fail before the
   implementation exists, then pass after.
5. Run `slcr vet` for structural errors, then `slcr check` for the result.

**Materialization.** Turning an acceptance criterion into validators is called
materializing it. One criterion may need several validators, for example the
same authentication check in two handler directories. Every validator for a
requirement must pass for its leaf to be satisfied.

When you rename or move a test, update the validator that names it, or
`slcr check` goes red. When a requirement or criterion changes meaning,
revisit the validators that cite it.

## What `slcr check` does

1. Installs any pinned descriptor the store lacks, unless `--offline`.
2. Vets the project. If vetting fails, nothing runs.
3. Runs every validator, many at once (`-j N`), and reports them in file and
   position order: `src/.validators.slcr.toml validator 1 (shop/REQ-002,
   sh): red (exit 1)`.
4. Evaluates each spec and prints a summary line, such as `shop: 5
   satisfied, 2 unsatisfied, 0 unknown (1 coverage gap)`.

Each red validator is followed by the last lines of its stderr and stdout.
`--format json` carries all of the captured output. Read it before you
change code.

`check` fails when vetting fails, when any validator can't be started (a
spawn failure), or when an unsatisfied MUST or MUST NOT isn't absorbed. With
`--deny-warnings`, it also fails on warnings: vet's warnings, unsatisfied
SHOULD and SHOULD NOT requirements, and absorbed failures. Unknown
requirements and MAY failures never fail it.

| You see | It means | Do |
|---|---|---|
| red validator | the check ran and failed | fix the code, or the validator if it's wrong |
| spawn failure | the tool couldn't start: not on PATH, a bad descriptor, a missing working directory | fix the environment. Don't change product code for it. |
| unknown | no validator for a leaf requirement | a coverage gap. Add a validator if proving that requirement is your task. |
| absorbed failure | it failed, but everything it refines is still satisfied | a warning. Usually one branch of an OR. |

## Fresh clones, CI, and sandboxes

- `slcr tools install` prepares a fresh clone or an offline sandbox. After it,
  `slcr vet` and `slcr check --offline` need no network.
- `slcr vet` never fetches, so a descriptor missing from the store is a `vet`
  error whose help says to run `slcr tools install`.
- A typical CI pipeline runs `slcr vet --deny-warnings`,
  `slcr render --expect-no-diff`, and `slcr check --deny-warnings`. Reproduce
  a failing CI job locally with the same command before you change anything.
