# Writing SLCR specifications

How to author and edit an SLCR spec file. The JSON Schema for the format is the
source of truth for field names and grammar. `slcr vet` enforces it plus the
invariants a schema can't express, so run `slcr vet` after every edit and let
it settle any doubt.

## Contents

1. [The file](#the-file)
2. [Node kinds and fields](#node-kinds-and-fields)
3. [IDs](#ids)
4. [Containment and the four edges](#containment-and-the-four-edges)
5. [Refinement and satisfaction](#refinement-and-satisfaction)
6. [Modality](#modality)
7. [Acceptance criteria](#acceptance-criteria)
8. [Writing good requirements](#writing-good-requirements)
9. [Canonical form rules](#canonical-form-rules)
10. [A complete example](#a-complete-example)
11. [Fixing `vet` findings](#fixing-vet-findings)

## The file

A spec is one file in TOML, YAML, or JSON, and its extension picks the format:
`.toml`, `.yaml`/`.yml`, or `.json`. The well-known name is `SPEC.slcr.toml`.
Create a new spec with `slcr init` instead of writing one from scratch: it
writes a valid skeleton and registers it in `SLCR.toml`.

Top level:

| Field | Meaning |
|---|---|
| `$schema` | The schema URL. `slcr init` writes it, so keep it. |
| `formatVersion` | Always the string `"1"`. |
| `spec.name` | The spec's name: lowercase letters and digits in dash-separated words (`todo-api`). Validators qualify requirement IDs with it (`todo-api/REQ-002`), so renaming a spec breaks every validator that refers to it. |
| `root` | The root Section (`SEC-001` from `init`). Every other node except Terms is nested under it. |
| `glossary` | The Glossary Section (`SEC-002` from `init`), which holds every Term in `terms`. |

The `contains` tree is the nesting itself: Sections and Requirements go in a
`children` array, and array position is sibling order. Nodes have no `order`
field. Every node has a `kind`: `section`, `requirement`,
`acceptanceCriterion`, or `term`.

## Node kinds and fields

Every node has `kind`, `id`, and `title`. A title is one non-empty line with
no leading or trailing whitespace. Prose fields (`body`, `rationale`,
`definition`) are non-empty Markdown and are rendered verbatim. Unknown fields
are errors at every level.

**Section** (`kind = "section"`): document structure and freeform prose
(preamble, background, motivation). A Section has no satisfaction value.

- `body` (optional): background that every slice under this Section inherits.
- `usesTerm` (optional): Term IDs.
- `children` (optional): Sections and Requirements.

**Requirement** (`kind = "requirement"`): one satisfiable statement.

- `body` (required): the statement, including its modality keyword.
- `rationale` (optional): why it exists. Keep it separate from `body`.
- `modality` (required): `MUST`, `MUST NOT`, `SHOULD`, `SHOULD NOT`, or `MAY`.
- `refinement`: `AND`, `OR`, or `XOR`. Required exactly when another
  requirement refines this one, and omitted otherwise.
- `refines` (optional): IDs of requirements this one helps satisfy.
- `dependsOn` (optional): IDs of requirements or Sections needed to understand
  or implement this one.
- `usesTerm` (optional): Term IDs.
- `acceptanceCriteria` (optional): Acceptance Criterion nodes.
- `children` (optional): nested Requirements only.

**Acceptance Criterion** (`kind = "acceptanceCriterion"`): what "done" looks
like for its requirement. It has `body` (required) and no children.

**Term** (`kind = "term"`): a glossary entry, inside `glossary.terms` only.

- `definition` (required).
- `aliases` (optional): other titles for the term.

## IDs

- Prefixes: `SEC-`, `REQ-`, `AC-`, `TERM-`. The number is exactly three
  zero-padded digits up to 999, and unpadded from 1000 on (`REQ-042`,
  `REQ-1000`). The prefix must agree with `kind`.
- Each ID is unique in the file. Numbers count up per kind across the whole
  spec: acceptance criteria are `AC-001`, `AC-002`, ... across all
  requirements, not per requirement.
- **Take the next free number. Never renumber, and never reuse a deleted ID.**
  Validators in other files refer to requirements and criteria by ID, and an
  ID that changes meaning silently re-targets them. To find the highest ID of
  a kind:
  ```bash
  slcr query "MATCH (n) WHERE n.id STARTS WITH 'REQ-' RETURN n.id ORDER BY n.id DESC LIMIT 1"
  ```
  Change the prefix for each kind (`AC-`, `SEC-`, `TERM-`). Moving a node to
  another place in the tree never changes its ID.

## Containment and the four edges

| Relation | From → To | Means | Affects satisfaction? | Pulled into a slice as |
|---|---|---|---|---|
| containment (nesting) | Section → Section/Requirement; Requirement → Requirement/criterion | where it renders, under which heading | **never** | ancestors' bodies |
| `refines` | Requirement → Requirement | "I help satisfy that" | **the only input** | refinement descendants, plus extra parents (body only) |
| `dependsOn` | Requirement → Requirement or Section | "you need that to understand or implement me" | never | transitive closure, with each target's ancestors' bodies |
| `usesTerm` | Requirement/Section → Term | "I use this word" | never | Glossary entries |

The rule that trips people up is that **nesting never implies refinement**.
Write every `refines` edge explicitly, including the one to a requirement's
own container. Nest without `refines` only when the child elaborates the
parent's intent without helping satisfy it. That child is a leaf judged by
its own validators.

A requirement that serves several parents (for example, "the request is
authenticated" under both GET and POST) lives in **one** place in the tree and
lists **every** parent in `refines`. Don't write it twice, because two copies
drift apart.

`refines` and `dependsOn` must be acyclic. `dependsOn` is for context, such as
the controller needing the endpoint's shapes and the table's columns. If you
want a child's result to count toward a parent, use `refines`.

## Refinement and satisfaction

`slcr check` evaluates each requirement as satisfied, unsatisfied, or unknown,
from the leaves up, over `refines` edges only:

- **Leaf** (nothing refines it): the AND of its own validators. With no
  validators it's **unknown**, which is a coverage gap, not a failure.
- **AND**: every refiner must be satisfied. MAY refiners are left out, so an
  optional child can't block its parent. If every refiner is MAY, the AND is
  vacuously satisfied and `vet` warns `vacuous-and`.
- **OR**: at least one refiner is satisfied (MAY refiners count).
- **XOR**: exactly one refiner is satisfied and the rest are unsatisfied.
- A requirement's own validators are ANDed with its refiners' result.
- Unknowns propagate under strong Kleene logic: a result is decided only when
  every way the unknowns could turn out agrees.

Choose the operator deliberately, because it changes what the spec means. "The
API accepts OAuth tokens or API keys" is `refinement = "OR"` over two `MAY`
children. "...and API keys" is `AND` over two `MUST` children. That
difference lives in data, not wording.

## Modality

`modality` sets **how much a failure matters**, not whether a check passed:

| Modality | Unsatisfied means |
|---|---|
| `MUST`, `MUST NOT` | a violation, which fails `check` |
| `SHOULD`, `SHOULD NOT` | a warning, which fails only with `--deny-warnings` |
| `MAY` | informational, never fails |

A failure is *absorbed* and downgraded to a warning when every requirement it
refines is still satisfied, such as one branch of a satisfied OR.

The body must contain the modality keyword in capitals, the way RFC 2119 uses
it. Otherwise `vet` warns `modality-not-stated`, and CI usually denies
warnings. `MUST NOT` and `SHOULD NOT` need no special structure: their
validators check that the thing is absent.

## Acceptance criteria

A criterion says what "done" will look like for one requirement. It should be
concrete enough that someone could write a check for it, but written **before
the code exists**, so it names no files, functions, or code patterns. Criteria
assert nothing themselves: the requirement is the assertion, and validators
prove it.

Write requirements first. Add criteria in a second pass, one requirement at a
time, from that requirement's slice (`slcr slice REQ-ID`), so each criterion
is written from exactly the context an implementer will have. A requirement
without criteria is valid. A validator may name the criterion it proves
(`criterion = "AC-004"`), and that criterion must belong to the validator's
own requirement.

## Writing good requirements

- **One testable statement per requirement.** If you write "and" between two
  behaviors, you probably want two requirements under a parent with
  `refinement = "AND"`.
- **Put background where slices find it.** A slice carries the bodies of a
  requirement's ancestors and of the things it depends on, never its
  siblings. Background in a sibling Section is invisible to the agent
  implementing the requirement, so move it into a common ancestor's `body` or
  point at it with `dependsOn`.
- **Structure follows meaning.** Group requirements under Sections the way a
  reader would look for them (by component, layer, or command). Don't add a
  field to record what the tree already says.
- **Make each one checkable.** Ask how a validator could prove it. If nothing
  could, the requirement is too vague.
- **State the why in `rationale`**, not in `body`.
- **Define words once.** Put a domain term in the Glossary (Terms in ID
  order) and link it with `usesTerm` from the nodes that use it. Prose is
  never auto-linked.
- **Keep a parent's body a real statement.** A parent's body says what it
  means for the whole to hold ("`slcr` MUST have an `init` subcommand that
  ..."), and its refiners say the parts.

## Canonical form rules

The format has exactly one valid spelling per graph:

- Omit optional fields that are empty. An empty array, an empty string, or
  `null` is an error, not a stand-in for "absent". This includes `refinement`
  on a leaf.
- Arrays of IDs (`refines`, `dependsOn`, `usesTerm`) are non-empty and have no
  duplicates.
- `glossary.terms` are in ID order.
- Field names are camelCase: `dependsOn`, `usesTerm`, `acceptanceCriteria`,
  `formatVersion`.

**TOML nesting.** Deeply nested specs use arrays of tables, and each header
attaches to the most recently opened parent:

```toml
[[root.children]]                      # a Section under the root
kind = "section"
id = "SEC-003"
title = "Controllers"

[[root.children.children]]             # a Requirement under SEC-003
kind = "requirement"
id = "REQ-005"
# ...

[[root.children.children.acceptanceCriteria]]   # a criterion of REQ-005
kind = "acceptanceCriterion"
id = "AC-002"
# ...

[[root.children.children.children]]    # a Requirement nested under REQ-005
kind = "requirement"
id = "REQ-006"
# ...
```

Order matters. A new `[[root.children]]` closes everything opened under the
previous one, so to add a child to an earlier node, insert it right after
that node's existing children. Don't append it at the end of the file. If a
node lands under the wrong parent, `slcr render --output -` or
`slcr slice REQ-ID` shows where it actually went.

## A complete example

The canon example in YAML, abridged. `slcr query` and `slcr slice` work on it
the same way in any format.

```yaml
$schema: https://slcr.io/reference/schemas/spec/v1.schema.json
formatVersion: "1"
spec:
  name: todo-api
root:
  kind: section
  id: SEC-001
  title: TodoList API
  body: A REST web application in MVC layout with one resource.
  children:
    - kind: section
      id: SEC-002
      title: Views
      body: The API surface.
      children:
        - kind: requirement
          id: REQ-001
          title: TodoListItem endpoint
          body: There MUST be an endpoint for the TodoListItem resource.
          modality: MUST
          refinement: AND
          children:
            - kind: requirement
              id: REQ-002
              title: POST creates an item
              body: The endpoint MUST accept POST to create an item.
              modality: MUST
              refinement: AND
              refines: [REQ-001]
              children:
                - kind: requirement          # one home, two refines parents
                  id: REQ-004
                  title: Request is authenticated
                  body: The request MUST be authenticated.
                  modality: MUST
                  refines: [REQ-002, REQ-003]
            - kind: requirement
              id: REQ-003
              title: GET reads an item
              body: The endpoint MUST accept GET to read an item.
              modality: MUST
              refinement: AND
              refines: [REQ-001]
    - kind: section
      id: SEC-003
      title: Controllers
      children:
        - kind: requirement
          id: REQ-005
          title: TodoListItem controller
          body: There MUST be a controller for TodoListItem.
          modality: MUST
          refinement: AND
          dependsOn: [REQ-001, REQ-007]      # context, not satisfaction
          children:
            - kind: requirement
              id: REQ-006
              title: Validates the request payload
              body: The controller MUST validate the request payload.
              modality: MUST
              refines: [REQ-005]
    - kind: section
      id: SEC-004
      title: Models
      body: Persistence assumptions.
      children:
        - kind: requirement
          id: REQ-007
          title: Table todo_list_item exists
          body: There MUST be a table `todo_list_item` with columns `title` (string) and `done` (bool).
          modality: MUST
          usesTerm: [TERM-001]
          acceptanceCriteria:
            - kind: acceptanceCriterion
              id: AC-001
              title: Table schema is declared
              body: A table named `todo_list_item` is declared with a string column `title` and a boolean column `done`. A structural match against the schema or migration source is sufficient.
    - kind: section
      id: SEC-005
      title: Authentication
      children:
        - kind: requirement
          id: REQ-008
          title: Accepts authentication tokens
          body: The API MUST accept OAuth tokens or API keys.
          modality: MUST
          refinement: OR
          children:
            - kind: requirement
              id: REQ-009
              title: Accepts OAuth tokens
              body: The API MAY accept OAuth tokens.
              modality: MAY
              refines: [REQ-008]
            - kind: requirement
              id: REQ-010
              title: Accepts API keys
              body: The API MAY accept API keys.
              modality: MAY
              refines: [REQ-008]
glossary:
  kind: section
  id: SEC-006
  title: Glossary
  terms:
    - kind: term
      id: TERM-001
      title: TodoListItem
      definition: A single item on a to-do list, with a title and a done flag.
```

`slcr slice REQ-005` on this spec gives the controller's implementer SEC-001
and SEC-003 (ancestors), REQ-006 (its refinement), REQ-001 and REQ-007 with
their Sections' bodies and AC-001 (dependencies), and TERM-001. It leaves out
authentication entirely, because that's someone else's job.

## Fixing `vet` findings

`slcr vet` labels each finding in the file's source. The common ones:

| Finding (`--format json` code) | Fix |
|---|---|
| parse error at a location | Wrong field name or type, an unknown field, an empty optional field, a bad ID spelling, or `kind` not matching the prefix. Read the label. |
| `duplicate-id` | Give the newer node the next free ID. |
| `dangling-reference` | A `refines`, `dependsOn`, or `usesTerm` names an ID that doesn't exist. Fix the ID, or add the node. |
| `cycle` | Break the `refines` or `dependsOn` cycle the finding prints. |
| `missing-refinement` | Something refines this requirement: set `refinement` to `AND`, `OR`, or `XOR`. |
| `unexpected-refinement` | Nothing refines this requirement: remove `refinement`, or add the missing `refines` edges on its children. |
| `term-out-of-order` | Reorder `glossary.terms` by ID. |
| `modality-not-stated` (warning) | Put the modality keyword in the body, in capitals. |
| `vacuous-and` (warning) | Every refiner of an AND is MAY. Use `OR`, or make a refiner MUST or SHOULD. |
