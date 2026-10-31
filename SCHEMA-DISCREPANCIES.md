# SLCR specification schema: documented vs. implemented

**Date:** 2026-10-31
**Scope:** the specification graph format (`*.slcr.toml|yaml|json`). The manifest,
tool descriptor, and validator file schemas are out of scope.

## Summary

The slcr implementation follows the documented JSON Schema closely. Every node kind,
field, ID pattern, and listed invariant has a counterpart in the code, and most of them
match exactly. This review found seven discrepancies:

| # | Discrepancy | Kind | Severity | Side to change |
|---|---|---|---|---|
| 1 | YAML specs accept numbers and booleans where the schema requires strings | Behavior | **High** | Implementation |
| 2 | `$schema`/`$id` URL differs between Notion and code, and neither URL resolves | Behavior | **Medium** | Both, plus publishing |
| 3 | The modality-keyword warning is stricter than the schema's wording | Behavior | Low | Documentation |
| 4 | ID numbers are capped at 2³² − 1; the schema has no cap | Behavior | Low | Either (decision needed) |
| 5 | The vacuous-AND invariant says "children"; the code checks refiners | Wording | Low | Documentation |
| 6 | `spec.name` is described as an open question; the code has settled it | Wording | Low | Documentation |
| 7 | The schema `$id` host (`slcr.io`) is out of date | Wording | Low | Documentation |

Item 1 is the only finding where a document can pass `slcr vet` while failing the
published schema. Items 3–7 need no code change.

## Sources and method

| Side | Source | Version |
|---|---|---|
| Documented | `slcr-spec.schema.json`, attached to the **Schemas** page in Notion (section "Specification graph", status "reviewed 2026-09-28") | Page last edited 2026-10-12 |
| Documented | `todo-api.spec.json`, the canon example on the same page | Same |
| Implemented | `wack/slcr`, mainly `src/spec/` (`graph.rs`, `node.rs`, `id.rs`, `text.rs`, `optional.rs`, `tagged.rs`, `vet.rs`) and `src/fs/format.rs` | Commit `a353622` |
| Implemented | `slcr` 0.1.0 release binary and a debug build of `a353622` | Both behave the same on every probe below |

The method had three parts:

1. Compared the schema with the code field by field and pattern by pattern.
2. Matched each `x-vetInvariants` entry to the `Violation` or `Warning` that enforces it.
3. Ran `slcr vet` and `slcr render` on small documents built to sit on each boundary
   found in steps 1 and 2.

The slcr repository's `CLAUDE.md` names the Notion page as "the source of truth for
field names, grammar, and the `x-vetInvariants` list." Where the two disagree, this
report treats the schema as the reference and states whether the code or the page
should change.

Line references to the schema use the downloaded `slcr-spec.schema.json`. Line
references to code are at commit `a353622`.

---

## 1. YAML accepts numbers and booleans in string fields

**Kind:** behavior · **Severity:** High · **Fix in:** implementation

### Documented

Every text field in the schema has `"type": "string"`, either directly or through
`$defs/title` and `$defs/markdown` (schema lines 47–56). `formatVersion` is
`"const": "1"`, a string (line 13). The page says the schema applies to whichever
format a file uses. YAML 1.2's core schema reads an unquoted `1` or `2024` as an
integer and `true` as a boolean, so a YAML document that writes them unquoted in these
fields does not conform.

### Implemented

`Format::parse` reads YAML with `serde_saphyr::from_str` and default options
(`src/fs/format.rs:45`). By default, serde-saphyr 1.3 stores a plain scalar's text in
any `String` field, whatever type the scalar would otherwise resolve to. The library
documents this on its `no_schema` option: "a number or boolean will be stored in the
string field exactly as provided."

TOML and JSON are strict. The same values in those formats fail to parse with
`wanted string or table` and `invalid type: integer`, respectively.

### Reproduction

```yaml
# p.yaml
formatVersion: 1          # schema: must be the string "1"
spec:
  name: probe
root:
  kind: section
  id: SEC-001
  title: true             # schema: must be a string
  children:
    - kind: requirement
      id: REQ-001
      title: 2024         # schema: must be a string
      body: 42            # schema: must be a string
      modality: MUST
glossary:
  kind: section
  id: SEC-002
  title: Glossary
```

```text
$ slcr vet p.yaml
  ⚠ the body of REQ-001 does not contain its modality keyword, MUST
p.yaml: 1 warning

$ slcr render p.yaml -o -
<a id="SEC-001"></a>
# true

<a id="REQ-001"></a>
## 1. 2024
```

The only finding is the modality warning. Nothing reports the type errors, and
`render` uses the values as text.

### Impact

- **The formats disagree.** The same content is valid YAML to `slcr` but invalid TOML
  and invalid JSON. A spec converted from YAML to another format can stop loading.
- **The schema and the tool disagree.** An editor or CI step that validates YAML
  against the published schema rejects files that `slcr vet` reports as clean, and the
  reverse never happens.
- **Mistakes pass silently.** A version number left unquoted, or a title or body that
  is accidentally a bare number or boolean, is read as text instead of being reported.

### Recommendation

Turn on serde-saphyr's `no_schema` option in `Format::parse`. That option rejects
unquoted scalars that would resolve to a number or boolean when they are read into a
string field. Also consider `strict_booleans`, so that the YAML 1.1 forms
(`yes`/`no`/`on`/`off`) are not treated as booleans, which matches YAML 1.2.

Verify these options before adopting them:

- Neither option has been tested against this codebase yet.
- Check that quoted values (`formatVersion: "1"`) still parse.
- Check that error spans still point at the offending scalar.
- Validator files and other YAML that goes through `Format::parse` would get the same
  stricter reading.

Add a unit test per affected field, and a CLI snapshot of the error.

---

## 2. The `$schema` URL differs, and neither URL resolves

**Kind:** behavior · **Severity:** Medium · **Fix in:** both, plus publishing the schema

### Documented

The schema's `$id` is `https://slcr.io/reference/schemas/spec/v1.schema.json` (line 3).
The page says these `slcr.io` URLs are placeholders. Notion's canon example sets
`$schema` to the same `slcr.io` URL.

### Implemented

`SCHEMA_URL` is `https://slcr.dev/reference/schemas/spec/v1.schema.json`
(`src/spec/graph.rs:12`). `SlcrRequirementsDocument::new` writes it into every new
document (`graph.rs:51`), so every spec created by `slcr init` names it. Clemente's new
`SPEC.slcr.toml` does too.

The repository's copy of the canon example, `tests/fixtures/todo-api.spec.json`,
differs from Notion's copy only on this line. Two tests depend on that copy:
`the_canon_example_deserializes` (`graph.rs:200`) and
`a_new_document_names_the_schema_the_canon_example_does` (`graph.rs:414`).

### Observed

| URL | HTTP status |
|---|---|
| `https://slcr.dev/reference/schemas/spec/v1.schema.json` | 404 |
| `https://slcr.io/reference/schemas/spec/v1.schema.json` | 404 |

The slcr.dev documentation has a Specification reference page
(`/docs/reference/specification`), but it doesn't link to a schema file.

### Impact

`$schema` is there so editors can validate and autocomplete specs. Every spec `slcr`
creates points at a URL that returns an HTML 404 page, so editors that follow it show
a fetch error or nothing.

Changing this URL later is expensive. It is written into user files, it is the `$id`
other schemas would reference, and nothing marks it as provisional. The longer `init`
keeps writing it, the more files carry it.

### Recommendation

1. Choose the canonical host. The code uses `slcr.dev`, where the documentation site
   already runs.
2. Publish the schema at that exact URL, served as `application/schema+json` or
   `application/json`.
3. Update the schema's `$id`, the page's placeholder note, and Notion's copy of the
   canon example to match, so that the repository fixture is byte-identical to the
   Notion example again.
4. Add a check, in CI or the website's build, that the published file's `$id` equals
   `SCHEMA_URL`.

---

## 3. The modality-keyword warning is stricter than the schema says

**Kind:** behavior · **Severity:** Low · **Fix in:** documentation

### Documented

`x-vetInvariants` (line 198): "A requirement's body contains its modality keyword
(warning)." The description of `body` (line 104) says the same thing.

### Implemented

`states_modality` (`src/spec/vet.rs:566–587`) splits the body into alphanumeric words
and looks for the modality as a whole phrase. "MUST" followed by "NOT" counts as
MUST NOT, not MUST, and the reverse is also true. So:

| `modality` | `body` | Literal "contains"? | `slcr vet` |
|---|---|---|---|
| `MUST` | `It MUST work.` | yes | ok |
| `MUST` | `It MUST NOT fail.` | yes (`MUST` is a substring) | **warning** |
| `MUST` | `It MUSTn't fail.` | yes | **warning** |
| `MUST NOT` | `It MUST NOT fail.` | yes | ok |

### Assessment

The code is better than the documented rule: a MUST requirement whose body only says
"MUST NOT" is almost certainly mislabeled. The code's doc comment says this is
deliberate. Only the schema's description is out of date.

### Recommendation

Reword the invariant, for example: "A requirement's body states its modality keyword as
a whole phrase, in capitals; `MUST NOT` does not state `MUST`, nor the reverse
(warning)." Update the `body` description to match.

---

## 4. ID numbers have an upper bound that the schema lacks

**Kind:** behavior · **Severity:** Low · **Fix in:** either (needs a decision)

### Documented

ID patterns such as `^REQ-(?:[0-9]{3}|[1-9][0-9]{3,})$` (lines 38–44) accept any
number of digits.

### Implemented

`Id<K>` stores the number as a `u32` (`src/spec/id.rs:74–77`), and `from_str` rejects
any number that overflows it (`id.rs:126–127`). The test
`overflowing_numbers_are_rejected` (`id.rs:278`) checks this on purpose.

```text
$ slcr vet p.yaml          # id: REQ-4294967296
  × could not parse p.yaml as YAML
  ╰── `REQ-4294967296` is not a valid requirement ID
```

### Impact

No real specification will reach this limit. Two smaller issues remain:

- A document the schema accepts is rejected by the tool.
- The error says the ID is "not a valid requirement ID", and its help text describes
  the pattern, which this ID matches. Someone who hits this limit gets a misleading
  explanation.

### Recommendation

Either:

- add `maxLength` to the ID patterns (or a bounded quantifier) in the schema, or list
  the bound in `x-vetInvariants`; **or**
- keep the schema as it is and give overflow its own error message, such as "ID number
  exceeds 4294967295".

The first option is simpler and makes the schema exact.

---

## 5. The vacuous-AND invariant says "children"; the code checks refiners

**Kind:** wording · **Severity:** Low · **Fix in:** documentation

### Documented

`x-vetInvariants` (line 199): "An AND requirement whose **refining children** are all
MAY is vacuously satisfied (warning)."

### Implemented

`warnings` (`src/spec/vet.rs:229–236`) checks every requirement that lists this one in
`refines`, wherever it sits in the tree, using `refiners` (`vet.rs:518–528`).
Containment `children` play no part.

### Assessment

The code is correct. The schema itself says that "nesting never implies refinement"
(schema description, and `children` at line 131). "Children" is the schema's word for
containment, so the invariant as written could be read as checking nested requirements,
which would be wrong.

### Recommendation

Reword the invariant: "An AND requirement all of whose refiners (the requirements whose
`refines` names it) are MAY is vacuously satisfied (warning)."

---

## 6. `spec.name` is described as an open question

**Kind:** wording · **Severity:** Low · **Fix in:** documentation

### Documented

`spec.name` (line 21): "the qualifier in references such as `todo-api/REQ-002`
(qualifier scheme still open)."

### Implemented

The qualifier scheme is decided. `QualifiedRequirementId` (`src/project/qualified.rs`)
is a `SpecName`, `/`, and a `RequirementId`. Validator files require it, and the
project index resolves it by spec name, never by path. The validator file schema on the
same Notion page already describes requirements as "always-qualified".

### Recommendation

Remove "(qualifier scheme still open)". Optionally add that the name must be unique
within a project, which `slcr vet` enforces as `DuplicateSpecName`.

---

## 7. The schema `$id` host is out of date

**Kind:** wording · **Severity:** Low · **Fix in:** documentation

The Schemas page introduction and the Open Questions page both say the schema `$id`
URLs live under `https://slcr.io/reference/schemas/`. The code and the documentation
site use `slcr.dev`. This is the documentation side of finding 2, and it should be
fixed together with it.

---

## Where the schema and code agree

These parts were checked and match. Each item names where the code enforces it.

**Document and nodes**

- Top-level required fields, `additionalProperties: false`, and an optional `$schema`
  (`graph.rs:23–43`).
- `formatVersion` is the constant `"1"` in TOML and JSON. YAML is the exception in
  finding 1.
- `spec` accepts only `name`, and the name matches `^[a-z0-9]+(?:-[a-z0-9]+)*$`
  (`text.rs:106–113`).
- `section`, `requirement`, `acceptanceCriterion`, `glossary`, and `term` have the
  required fields, optional fields, and unknown-field rejection the schema gives them.
  Each `kind` constant is checked when reading, and both the root and the Glossary must
  have `kind: section` (`node.rs`, `tagged.rs`).
- What can contain what:
  - Sections contain sections and requirements.
  - Requirements contain only requirements.
  - The Glossary contains only terms and has no `children` or `usesTerm`.
  - Acceptance criteria appear only under `acceptanceCriteria`.
- `modality` and `refinement` accept exactly the schema's values, in capitals.

**Values and patterns**

- ID patterns, including the one canonical spelling, `000`, and four or more digits
  without padding (`id.rs:105–129`). The upper bound in finding 4 is the only
  difference.
- `dependsOn` accepts only `SEC-` and `REQ-` IDs. `refines` accepts only `REQ-`.
  `usesTerm` accepts only `TERM-`.
- The title pattern `^\S(?:[^\r\n]*\S)?$`, including the ECMA-262 meaning of `\S`:
  U+0085 is not whitespace and U+FEFF is (`text.rs:28–60`).
- Markdown fields have `minLength: 1`.

**Arrays and nulls**

- `nonEmptySet` (`minItems: 1`, `uniqueItems: true`) for `refines`, `dependsOn`,
  `usesTerm`, and `aliases` (`optional.rs:38–51`).
- `minItems: 1` for `children`, `acceptanceCriteria`, and `terms` (`optional.rs:22–34`).
- An explicit `null` in any optional field is rejected, as the schema's types require.

**`x-vetInvariants`**

Each invariant maps to exactly one finding in `src/spec/vet.rs`:

| Invariant | Enforced by |
|---|---|
| Every id is unique across the document | `Violation::DuplicateId` |
| Every reference names an existing node of the allowed kind | `Violation::DanglingReference` (the kind is guaranteed by the ID's type) |
| `refines` is acyclic; `dependsOn` is acyclic | `Violation::Cycle` |
| `refinement` is present exactly when something refines the requirement | `Violation::MissingRefinement`, `Violation::UnexpectedRefinement` |
| Glossary terms are serialized in ID order | `Violation::TermOutOfOrder` |
| The body contains its modality keyword (warning) | `Warning::ModalityNotStated` (see finding 3) |
| An all-MAY AND is vacuously satisfied (warning) | `Warning::VacuousAnd` (see finding 5) |

## Considered and not counted as discrepancies

- **`$schema`'s `"format": "uri-reference"` is not checked.** In draft 2020-12,
  `format` is an annotation unless a validator opts in to checking it, so accepting any
  string conforms to the schema.
- **`dependsOn` can name the Glossary section** (`vet.rs:287`). The Glossary is a
  section, and the schema allows any `SEC-` ID.
- **Acceptance criterion IDs are unique across the whole document**, not just within
  their requirement. That is what "every id is unique across the document" says.
- **The schema doesn't require the root to be `SEC-001`, the Glossary to be `SEC-002`,
  or the Glossary's title to be "Glossary"**, and the code doesn't either. `slcr init`
  follows those conventions without enforcing them.

## Suggested next steps

1. **slcr code:** fix finding 1 (strict YAML scalars), with tests in `src/fs/format.rs`
   and a CLI snapshot.
2. **Decision, then publish:** settle the schema host, publish
   `spec/v1.schema.json` at `SCHEMA_URL`, and add a drift check (finding 2).
3. **Decision:** choose how to handle the ID bound (finding 4).
4. **Notion:** update the Schemas page for findings 3, 5, 6, and 7, and replace the
   canon example's `$schema`, so that `tests/fixtures/todo-api.spec.json` is again a
   byte-for-byte copy of it.
