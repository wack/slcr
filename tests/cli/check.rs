//! `slcr check`: installing missing descriptors, vetting, running every
//! validator, and evaluating each requirement from their results.
//!
//! The validators run `sh` and nothing else, found on PATH as any program
//! is, so these run on any Unix.

use super::{Workspace, pin, stored};

/// A shop's specification: checking out is the AND of a cart total and a
/// payment, which is the OR of paying by card or by voucher; a receipt
/// SHOULD be emailed, and gift wrap MAY be offered.
const SPEC: &str = r#"formatVersion: "1"
spec:
  name: shop
root:
  kind: section
  id: SEC-001
  title: Shop
  children:
    - kind: requirement
      id: REQ-001
      title: Checkout
      body: Checking out MUST work.
      modality: MUST
      refinement: AND
      children:
        - kind: requirement
          id: REQ-002
          title: Cart total
          body: The cart MUST total its items.
          modality: MUST
          refines: [REQ-001]
        - kind: requirement
          id: REQ-003
          title: Payment
          body: Payment MUST succeed by card or by voucher.
          modality: MUST
          refinement: OR
          refines: [REQ-001]
          children:
            - kind: requirement
              id: REQ-004
              title: Card payment
              body: Paying by card MUST succeed.
              modality: MUST
              refines: [REQ-003]
            - kind: requirement
              id: REQ-005
              title: Voucher payment
              body: Paying by voucher MUST succeed.
              modality: MUST
              refines: [REQ-003]
    - kind: requirement
      id: REQ-006
      title: Receipt
      body: A receipt SHOULD be emailed.
      modality: SHOULD
    - kind: requirement
      id: REQ-007
      title: Gift wrap
      body: Items MAY be gift wrapped.
      modality: MAY
glossary:
  kind: section
  id: SEC-002
  title: Glossary
"#;

/// A tool that runs `with.script` with `sh -c`, in the project root.
const SH: &str = r#"formatVersion = "1"
name = "sh"
version = "1.0.0"

[validator.schema]
type = "object"
required = ["script"]
additionalProperties = false
properties.script = { type = "string" }

[invoke]
argv = ["sh", "-c", "{with.script}"]
cwd = "project"

[results]
pass = [0]
"#;

/// A tool whose program is on no PATH.
const MISSING: &str = r#"formatVersion = "1"
name = "missing"
version = "1.0.0"

[validator.schema]
type = "object"

[invoke]
argv = ["slcr-no-such-program"]
cwd = "project"

[results]
pass = [0]
"#;

/// The manifest's entry for the tool `key`, whose descriptor, `text`, is
/// kept in the project at `tools/<key>.toml`.
fn entry(key: &str, text: &str) -> String {
    format!(
        "\n[tools.{key}]\nversion = \"1.0.0\"\nsource = {{ path = \"tools/{key}.toml\" }}\n\
         pin = \"{}\"\n",
        pin(text)
    )
}

/// A project with [SPEC], declaring the `sh` tool, whose descriptor is
/// installed, and no validator file yet.
fn shop() -> Workspace {
    let workspace = Workspace::new();
    workspace
        .file(
            "SLCR.toml",
            format!(
                "formatVersion = \"1\"\nspecs = [\"SPEC.slcr.yml\"]\n{}",
                entry("sh", SH)
            ),
        )
        .file("SPEC.slcr.yml", SPEC)
        .file("tools/sh.toml", SH)
        .install(SH);
    workspace
}

/// A validator of `requirement` that runs `script`, in TOML.
fn validator(requirement: &str, script: &str) -> String {
    format!(
        "\n[[validator]]\nrequirement = \"shop/{requirement}\"\ntool = \"sh\"\n\
         with = {{ script = {script:?} }}\n"
    )
}

/// A validator file holding `validators`, in TOML.
fn validators(validators: &[String]) -> String {
    format!("formatVersion = \"1\"\n{}", validators.concat())
}

/// [shop], with a validator for every leaf: in `src/`, scripts `cart`,
/// `card`, and `voucher` for the cart total and the two payments; in
/// `src/mail/`, ones that pass for the receipt and gift wrap.
fn validated(cart: &str, card: &str, voucher: &str) -> Workspace {
    let workspace = shop();
    workspace
        .file(
            "src/.validators.slcr.toml",
            validators(&[
                validator("REQ-002", cart),
                validator("REQ-004", card),
                validator("REQ-005", voucher),
            ]),
        )
        .file(
            "src/mail/.validators.slcr.toml",
            validators(&[validator("REQ-006", "true"), validator("REQ-007", "true")]),
        );
    workspace
}

#[test]
fn check_help() {
    assert_cmd_snapshot!(Workspace::new().slcr(&["check", "--help"]));
}

#[test]
fn check_passes_a_project_whose_requirements_are_satisfied() {
    assert_cmd_snapshot!(validated("true", "true", "true").slcr(&["check"]));
}

#[test]
fn check_names_files_from_the_working_directory() {
    let workspace = validated("true", "true", "true");
    assert_cmd_snapshot!(workspace.slcr_in("src/mail", &["check"]));
}

#[test]
fn check_fails_an_unsatisfied_must() {
    assert_cmd_snapshot!(validated("exit 1", "true", "true").slcr(&["check"]));
}

#[test]
fn check_passes_an_absorbed_failure_unless_warnings_are_denied() {
    // Paying by card is enough to pay.
    let workspace = validated("true", "true", "exit 2");
    assert_cmd_snapshot!("allowed", workspace.slcr(&["check"]));
    assert_cmd_snapshot!("denied", workspace.slcr(&["check", "--deny-warnings"]));
}

#[test]
fn check_passes_an_unsatisfied_should_with_a_warning() {
    let workspace = validated("true", "true", "true");
    workspace.file(
        "src/mail/.validators.slcr.toml",
        validators(&[validator("REQ-006", "exit 1"), validator("REQ-007", "true")]),
    );
    assert_cmd_snapshot!(workspace.slcr(&["check"]));
}

#[test]
fn check_passes_an_unsatisfied_may() {
    let workspace = validated("true", "true", "true");
    workspace.file(
        "src/mail/.validators.slcr.toml",
        validators(&[validator("REQ-006", "true"), validator("REQ-007", "exit 1")]),
    );
    assert_cmd_snapshot!(workspace.slcr(&["check", "--deny-warnings"]));
}

#[test]
fn check_passes_unknown_requirements() {
    // Nothing validates paying by voucher, the receipt, or gift wrap.
    let workspace = shop();
    workspace.file(
        "src/.validators.slcr.toml",
        validators(&[validator("REQ-002", "true"), validator("REQ-004", "true")]),
    );
    assert_cmd_snapshot!(workspace.slcr(&["check", "--deny-warnings"]));
}

#[test]
fn check_shows_the_end_of_a_red_validator_s_output() {
    let script = "i=1; while [ $i -le 30 ]; do echo \"test $i ok\"; i=$((i+1)); done; \
                  echo 'assertion failed: total == 3' >&2; exit 101";
    assert_cmd_snapshot!(validated(script, "echo hidden; true", "true").slcr(&["check"]));
}

#[test]
fn check_reports_a_validator_that_cannot_be_run() {
    let workspace = validated("true", "true", "true");
    workspace
        .file(
            "SLCR.toml",
            format!(
                "formatVersion = \"1\"\nspecs = [\"SPEC.slcr.yml\"]\n{}{}",
                entry("missing", MISSING),
                entry("sh", SH)
            ),
        )
        .file("tools/missing.toml", MISSING)
        .install(MISSING)
        .file(
            "src/.validators.slcr.toml",
            validators(&[
                validator("REQ-002", "true"),
                validator("REQ-004", "true"),
                "\n[[validator]]\nrequirement = \"shop/REQ-005\"\ntool = \"missing\"\n".to_owned(),
            ]),
        );
    assert_cmd_snapshot!(workspace.slcr(&["check"]));
}

#[test]
fn check_runs_nothing_when_vetting_fails() {
    let workspace = shop();
    workspace.file(
        "src/.validators.slcr.toml",
        validators(&[
            validator("REQ-002", "touch ran"),
            validator("REQ-099", "touch ran"),
        ]),
    );
    assert_cmd_snapshot!(workspace.slcr(&["check"]));
    assert!(!workspace.path("ran").exists());
}

#[test]
fn check_installs_a_missing_descriptor() {
    let workspace = validated("true", "true", "true");
    std::fs::remove_file(workspace.path(&stored(SH))).unwrap();
    assert_cmd_snapshot!(workspace.slcr(&["check"]));
    assert_eq!(workspace.read(&stored(SH)), SH);
}

#[test]
fn check_fails_when_a_descriptor_cannot_be_installed() {
    let workspace = validated("touch ran", "true", "true");
    std::fs::remove_file(workspace.path(&stored(SH))).unwrap();
    std::fs::remove_file(workspace.path("tools/sh.toml")).unwrap();
    assert_cmd_snapshot!(workspace.slcr(&["check"]));
    assert!(!workspace.path("ran").exists());
}

#[test]
fn check_offline_never_installs_a_descriptor() {
    let workspace = validated("touch ran", "true", "true");
    std::fs::remove_file(workspace.path(&stored(SH))).unwrap();
    assert_cmd_snapshot!(workspace.slcr(&["check", "--offline"]));
    assert!(!workspace.path("ran").exists());
    assert!(!workspace.path(&stored(SH)).exists());
}

#[test]
fn check_reports_the_same_whatever_the_number_of_jobs() {
    // The first validators finish last when they run at once.
    let workspace = validated("sleep 0.6; exit 1", "sleep 0.3", "true");
    let output = |jobs: &str| workspace.slcr(&["check", "-j", jobs]).output().unwrap();
    let (one, four) = (output("1"), output("4"));
    assert_eq!(one.status.code(), four.status.code());
    assert_eq!(
        String::from_utf8_lossy(&one.stdout),
        String::from_utf8_lossy(&four.stdout)
    );
    assert_eq!(
        String::from_utf8_lossy(&one.stderr),
        String::from_utf8_lossy(&four.stderr)
    );
    assert_cmd_snapshot!(workspace.slcr(&["check", "--jobs", "4"]));
}

#[test]
fn check_rejects_zero_jobs() {
    assert_cmd_snapshot!(shop().slcr(&["check", "-j", "0"]));
}

#[test]
fn check_takes_no_files() {
    assert_cmd_snapshot!(shop().slcr(&["check", "SPEC.slcr.yml"]));
}

#[test]
fn check_reports_in_json() {
    let workspace = validated(
        "echo 'test cart::total ... FAILED'; echo 'expected 3' >&2; exit 101",
        "true",
        "exit 2",
    );
    let mut settings = insta::Settings::clone_current();
    settings.add_filter(r#""durationMs": \d+"#, r#""durationMs": "[DURATION]""#);
    settings.bind(|| {
        assert_cmd_snapshot!(workspace.slcr(&["check", "--format", "json"]));
    });
}

#[test]
fn check_reports_a_vetting_failure_in_json() {
    let workspace = shop();
    workspace.file(
        "src/.validators.slcr.toml",
        validators(&[validator("REQ-099", "touch ran")]),
    );
    assert_cmd_snapshot!(workspace.slcr(&["check", "--format", "json"]));
    assert!(!workspace.path("ran").exists());
}

#[test]
fn check_requires_a_project() {
    let workspace = Workspace::new();
    workspace.file("SPEC.slcr.yml", SPEC);
    assert_cmd_snapshot!(workspace.slcr(&["check"]));
}

/// Ctrl-C at a terminal reaches slcr, but not the validators, which run in
/// process groups of their own; slcr stops them itself.
#[cfg(target_os = "linux")]
#[test]
fn check_stops_every_validator_when_interrupted() {
    use std::time::{Duration, Instant};

    use rustix::process::{Pid, Signal, kill_process};

    // The second validator starts a grandchild, records its PID, and
    // would outlive slcr; the third never starts.
    let workspace = validated(
        "true",
        "sleep 30 & echo $! > pid.tmp; mv pid.tmp pid; wait",
        "touch ran",
    );
    let child = workspace
        .slcr(&["check", "-j", "1"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !workspace.path("pid").exists() {
        assert!(Instant::now() < deadline, "the validator never started");
        std::thread::sleep(Duration::from_millis(20));
    }
    kill_process(Pid::from_child(&child), Signal::INT).unwrap();
    let output = child.wait_with_output().unwrap();
    let shown = format!(
        "exit_code: {:?}\n----- stdout -----\n{}\n----- stderr -----\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    insta::assert_snapshot!(shown);
    assert!(!workspace.path("ran").exists());

    let pid = workspace.read("pid");
    let stat = std::path::Path::new("/proc").join(pid.trim()).join("stat");
    // Killed, it is gone, or a zombie until something reaps it.
    let dead = || {
        std::fs::read_to_string(&stat).map_or(true, |stat| {
            let state = stat
                .rsplit_once(") ")
                .and_then(|(_, rest)| rest.chars().next());
            matches!(state, Some('Z' | 'X'))
        })
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !dead() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(dead(), "the grandchild, {}, is still running", pid.trim());
}
