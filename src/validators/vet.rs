use std::collections::BTreeMap;

use super::{Finding, Problem, ValidatorFile};
use crate::project::{SpecIndex, index::UnresolvedRequirement};
use crate::tools::{ToolKey, manifest::ManifestFile};

/// What vetting a validator file needs from the rest of the project.
pub(crate) struct Context<'a> {
    /// The project's specifications, which validators name requirements
    /// of.
    pub(crate) specs: &'a SpecIndex<'a>,
    /// Whether `specs` holds every specification the manifest matches.
    /// When one couldn't be loaded, a validator that names a
    /// specification `specs` lacks may name that one, so it isn't
    /// reported.
    pub(crate) complete: bool,
    /// The manifest, which declares the tools validators run.
    pub(crate) manifest: &'a ManifestFile,
    /// The compiled validator schema of each declared tool whose
    /// descriptor could be loaded. A tool missing here gets no check of
    /// its validators' `with`.
    pub(crate) schemas: &'a BTreeMap<ToolKey, jsonschema::Validator>,
}

/// Every way `file`'s validators break the invariants of the validator
/// schema's `x-vetInvariants`, in the order of the validators:
///
/// 1. `requirement` names a specification of the project, and a
///    requirement in it.
/// 2. `criterion` names an acceptance criterion of that requirement.
/// 3. `tool` is a key of the manifest's `[tools]`.
/// 4. `with`, or an empty object if it is omitted, satisfies the validator
///    schema of the descriptor the tool pins: each schema error is a
///    problem of its own.
/// 5. No two validators are identical.
///
/// A check that depends on one that failed, such as the criterion of a
/// requirement that doesn't resolve, is skipped rather than reported too.
pub(crate) fn vet(file: &ValidatorFile, cx: &Context<'_>) -> Vec<Finding> {
    let mut findings = Vec::new();
    let validators = file.validators();
    for (index, validator) in validators.iter().enumerate() {
        let spans = file.spans(index);
        let mut report = |problem, span: Option<_>| {
            let span = span.or(spans.entry);
            findings.push(Finding::new(
                index + 1,
                problem,
                span,
                file.source().clone(),
            ));
        };

        match cx.specs.resolve(validator.requirement()) {
            Ok(resolved) => {
                if let Some(criterion) = validator.criterion()
                    && let Err(err) = resolved.criterion(criterion)
                {
                    report(Problem::Criterion(err), spans.criterion);
                }
            }
            Err(UnresolvedRequirement::UnknownSpec { .. }) if !cx.complete => {}
            Err(err) => report(Problem::Requirement(err), spans.requirement),
        }

        let tool = validator.tool();
        match cx.manifest.tool(tool) {
            Err(err) => report(Problem::Tool(err), spans.tool),
            Ok(_) => {
                if let Some(schema) = cx.schemas.get(tool) {
                    let with = validator.with_or_empty();
                    for error in schema.iter_errors(&with) {
                        let problem = Problem::InvalidWith {
                            tool: tool.clone(),
                            pointer: error.instance_path().to_string(),
                            reason: error.to_string(),
                        };
                        report(problem, spans.with.or(spans.tool));
                    }
                }
            }
        }

        let earlier = validators[..index]
            .iter()
            .position(|earlier| earlier.is_identical(validator));
        if let Some(first) = earlier {
            report(Problem::DuplicateValidator { first: first + 1 }, None);
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::project::{IndexedSpec, ProjectFile};
    use crate::spec::graph::UnvettedDocument;
    use crate::terminal::render_plain;
    use crate::tools::{descriptor::Descriptor, manifest::Project};
    use miette::Diagnostic;

    const TODO_API_JSON: &str = include_str!("../../tests/fixtures/todo-api.spec.json");
    const NEXTEST: &str = include_str!("../../tests/fixtures/cargo-nextest.descriptor.toml");

    /// A project with the canon example as `SPEC.slcr.json`, a manifest
    /// that declares cargo-nextest and jev, and one validator file.
    struct Fixture {
        dir: tempfile::TempDir,
        spec: (ProjectFile, UnvettedDocument),
        manifest: ManifestFile,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let pin = format!("blake3:{}", blake3::hash(b"").to_hex());
            let manifest = format!(
                "formatVersion = \"1\"\nspecs = [\"SPEC.slcr.json\"]\n\n[tools.cargo-nextest]\n\
                 version = \"1.0.0\"\nsource = {{ path = \"nextest.toml\" }}\npin = \"{pin}\"\n\n\
                 [tools.jev]\nversion = \"1.0.0\"\nsource = {{ path = \"jev.toml\" }}\npin = \"{pin}\"\n"
            );
            std::fs::write(dir.path().join("SLCR.toml"), manifest).unwrap();
            let manifest = ManifestFile::load(&Project::find_from(dir.path()).unwrap()).unwrap();
            let document = serde_json::from_str(TODO_API_JSON).unwrap();
            let file = ProjectFile::new(dir.path(), dir.path(), PathBuf::from("SPEC.slcr.json"));
            Self {
                dir,
                spec: (file, document),
                manifest,
            }
        }

        /// The findings in the validator file `source`, written in TOML,
        /// with cargo-nextest's schema known and jev's not.
        fn vet(&self, source: &str) -> Vec<Finding> {
            self.vet_with(source, true)
        }

        fn vet_with(&self, source: &str, complete: bool) -> Vec<Finding> {
            let path = self.dir.path().join("src/.validators.slcr.toml");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, source).unwrap();
            let file = ProjectFile::new(
                self.dir.path(),
                self.dir.path(),
                PathBuf::from("src/.validators.slcr.toml"),
            );
            let file = ValidatorFile::load(file).unwrap();
            let (specs, _) = SpecIndex::new([IndexedSpec {
                file: &self.spec.0,
                document: &self.spec.1,
            }]);
            let descriptor = Descriptor::parse("nextest.toml", NEXTEST).unwrap();
            let schemas = BTreeMap::from([(
                ToolKey::try_from("cargo-nextest".to_owned()).unwrap(),
                jsonschema::draft202012::new(descriptor.schema()).unwrap(),
            )]);
            let cx = Context {
                specs: &specs,
                complete,
                manifest: &self.manifest,
                schemas: &schemas,
            };
            vet(&file, &cx)
        }
    }

    /// A validator file of the validators `entries`, each the body of a
    /// `[[validator]]` table.
    fn validators(entries: &[&str]) -> String {
        let mut source = "formatVersion = \"1\"\n".to_owned();
        for entry in entries {
            source.push_str("\n[[validator]]\n");
            source.push_str(entry);
        }
        source
    }

    fn messages(findings: &[Finding]) -> Vec<String> {
        findings.iter().map(ToString::to_string).collect()
    }

    const NEXTEST_OK: &str = "requirement = \"todo-api/REQ-002\"\ntool = \"cargo-nextest\"\nwith = { test = \"a::b\" }\n";

    #[test]
    fn valid_validators_have_no_findings() {
        let source = validators(&[
            NEXTEST_OK,
            "requirement = \"todo-api/REQ-007\"\ncriterion = \"AC-001\"\ntool = \"jev\"\n\
             with = { anything = [1, 2] }\n",
            "requirement = \"todo-api/REQ-007\"\ntool = \"jev\"\n",
        ]);
        let fixture = Fixture::new();
        assert_eq!(messages(&fixture.vet(&source)), [] as [&str; 0]);
    }

    #[test]
    fn a_requirement_must_resolve() {
        let source = validators(&[
            "requirement = \"todo/REQ-002\"\ntool = \"jev\"\n",
            "requirement = \"todo-api/REQ-099\"\ncriterion = \"AC-001\"\ntool = \"jev\"\n",
        ]);
        let fixture = Fixture::new();
        let findings = fixture.vet(&source);
        assert_eq!(
            messages(&findings),
            [
                "validator 1: `todo/REQ-002` names the specification `todo`, which the project \
                 doesn't have",
                // The criterion of a requirement that doesn't resolve isn't
                // checked.
                "validator 2: `todo-api/REQ-099` names REQ-099, which the specification \
                 `todo-api` (SPEC.slcr.json) doesn't have",
            ]
        );
        assert_eq!(
            findings[0].help().unwrap().to_string(),
            "the project's specifications are: todo-api"
        );
    }

    #[test]
    fn an_unknown_spec_is_not_reported_when_a_spec_could_not_be_loaded() {
        let source = validators(&[
            "requirement = \"todo/REQ-002\"\ntool = \"jev\"\n",
            "requirement = \"todo-api/REQ-099\"\ntool = \"jev\"\n",
        ]);
        let fixture = Fixture::new();
        assert_eq!(
            messages(&fixture.vet_with(&source, false)),
            [
                "validator 2: `todo-api/REQ-099` names REQ-099, which the specification \
                 `todo-api` (SPEC.slcr.json) doesn't have"
            ]
        );
    }

    #[test]
    fn a_criterion_must_belong_to_its_requirement() {
        let source = validators(&[
            "requirement = \"todo-api/REQ-001\"\ncriterion = \"AC-001\"\ntool = \"jev\"\n",
        ]);
        let fixture = Fixture::new();
        let findings = fixture.vet(&source);
        assert_eq!(
            messages(&findings),
            [
                "validator 1: `AC-001` is not an acceptance criterion of `todo-api/REQ-001` \
                 (SPEC.slcr.json)"
            ]
        );
        assert_eq!(
            findings[0].help().unwrap().to_string(),
            "AC-001 belongs to REQ-007; REQ-001 has no acceptance criteria"
        );
    }

    #[test]
    fn a_tool_must_be_declared() {
        let source = validators(&[
            "requirement = \"todo-api/REQ-002\"\ntool = \"ast-grep\"\nwith = { rule = 1 }\n",
        ]);
        let fixture = Fixture::new();
        let findings = fixture.vet(&source);
        assert_eq!(
            messages(&findings),
            ["validator 1: SLCR.toml declares no tool named ast-grep"]
        );
        assert_eq!(
            findings[0].help().unwrap().to_string(),
            "the tools it declares are: cargo-nextest, jev"
        );
    }

    #[test]
    fn with_must_satisfy_the_tool_s_schema() {
        let source = validators(&[
            "requirement = \"todo-api/REQ-002\"\ntool = \"cargo-nextest\"\n",
            "requirement = \"todo-api/REQ-002\"\ntool = \"cargo-nextest\"\n\
             with = { test = \"\", extra = true }\n",
            "requirement = \"todo-api/REQ-002\"\ntool = \"cargo-nextest\"\nwith = { test = 1 }\n",
        ]);
        let fixture = Fixture::new();
        let findings = fixture.vet(&source);
        assert_eq!(
            messages(&findings),
            [
                "validator 1: `with` does not satisfy the validator schema of cargo-nextest: \
                 \"test\" is a required property",
                "validator 2: `with` at `/test` does not satisfy the validator schema of \
                 cargo-nextest: \"\" is shorter than 1 character",
                "validator 2: `with` does not satisfy the validator schema of cargo-nextest: \
                 Additional properties are not allowed ('extra' was unexpected)",
                "validator 3: `with` at `/test` does not satisfy the validator schema of \
                 cargo-nextest: 1 is not of type \"string\"",
            ]
        );
        let pointers: Vec<&str> = findings
            .iter()
            .map(|finding| match finding.problem() {
                Problem::InvalidWith { pointer, .. } => pointer.as_str(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(pointers, ["", "/test", "", "/test"]);
    }

    #[test]
    fn identical_validators_are_reported_after_the_first() {
        let source = validators(&[
            NEXTEST_OK,
            "requirement = \"todo-api/REQ-007\"\ntool = \"jev\"\n",
            "with = { test = \"a::b\" }\ntool = \"cargo-nextest\"\nrequirement = \"todo-api/REQ-002\"\n",
            "requirement = \"todo-api/REQ-007\"\ntool = \"jev\"\nwith = {}\n",
            NEXTEST_OK,
        ]);
        let fixture = Fixture::new();
        assert_eq!(
            messages(&fixture.vet(&source)),
            [
                "validator 3: it is identical to validator 1",
                "validator 4: it is identical to validator 2",
                "validator 5: it is identical to validator 1",
            ]
        );
    }

    #[test]
    fn every_problem_is_reported_in_validator_order() {
        let source = validators(&[
            "requirement = \"todo-api/REQ-099\"\ntool = \"curl\"\n",
            "requirement = \"todo-api/REQ-099\"\ntool = \"curl\"\n",
        ]);
        let fixture = Fixture::new();
        assert_eq!(
            messages(&fixture.vet(&source)),
            [
                "validator 1: `todo-api/REQ-099` names REQ-099, which the specification \
                 `todo-api` (SPEC.slcr.json) doesn't have",
                "validator 1: SLCR.toml declares no tool named curl",
                "validator 2: `todo-api/REQ-099` names REQ-099, which the specification \
                 `todo-api` (SPEC.slcr.json) doesn't have",
                "validator 2: SLCR.toml declares no tool named curl",
                "validator 2: it is identical to validator 1",
            ]
        );
    }

    #[test]
    fn findings_label_what_they_are_about() {
        let source = validators(&[
            "requirement = \"todo-api/REQ-001\"\ncriterion = \"AC-001\"\ntool = \"cargo-nextest\"\n\
             with = { test = 1 }\n",
            "requirement = \"todo-api/REQ-001\"\ncriterion = \"AC-001\"\ntool = \"cargo-nextest\"\n\
             with = { test = 1 }\n",
        ]);
        let fixture = Fixture::new();
        let findings = fixture.vet(&source);
        let labeled: Vec<&str> = findings
            .iter()
            .map(|finding| {
                let label = finding.labels().unwrap().next().unwrap();
                &source[label.offset()..label.offset() + label.len()]
            })
            .collect();
        assert_eq!(
            labeled,
            [
                "\"AC-001\"",
                "{ test = 1 }",
                "\"AC-001\"",
                "{ test = 1 }",
                "[[validator]]",
            ]
        );
        assert_eq!(
            render_plain(&findings[0]),
            "  × validator 1: `AC-001` is not an acceptance criterion of `todo-api/REQ-001`\n  \
             │ (SPEC.slcr.json)\n   \
             ╭─[src/.validators.slcr.toml:5:13]\n \
             4 │ requirement = \"todo-api/REQ-001\"\n \
             5 │ criterion = \"AC-001\"\n   \
             ·             ────────\n \
             6 │ tool = \"cargo-nextest\"\n   \
             ╰────\n  \
             help: AC-001 belongs to REQ-007; REQ-001 has no acceptance criteria\n"
        );
    }

    #[test]
    fn a_schema_error_without_with_labels_the_tool() {
        let source =
            validators(&["requirement = \"todo-api/REQ-002\"\ntool = \"cargo-nextest\"\n"]);
        let fixture = Fixture::new();
        let findings = fixture.vet(&source);
        let label = findings[0].labels().unwrap().next().unwrap();
        assert_eq!(
            &source[label.offset()..label.offset() + label.len()],
            "\"cargo-nextest\""
        );
    }
}
