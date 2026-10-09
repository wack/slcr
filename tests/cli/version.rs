//! `slcr version`.

use insta_cmd::assert_cmd_snapshot;

use super::Workspace;

#[test]
fn version_help() {
    assert_cmd_snapshot!(Workspace::new().slcr(&["version", "--help"]));
}

#[test]
fn version_prints_the_package_version() {
    let output = Workspace::new().slcr(&["version"]).output().unwrap();
    let expected = format!("v{}\n", env!("CARGO_PKG_VERSION"));
    assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);

    // Redacted, so that a release doesn't change the snapshot.
    let mut settings = insta::Settings::clone_current();
    settings.add_filter(&regex_escape(env!("CARGO_PKG_VERSION")), "[VERSION]");
    settings.bind(|| {
        assert_cmd_snapshot!(Workspace::new().slcr(&["version"]));
    });
}

#[test]
fn version_takes_no_arguments() {
    assert_cmd_snapshot!(Workspace::new().slcr(&["version", "extra"]));
}

/// `text` with the regex metacharacters a version can hold escaped.
fn regex_escape(text: &str) -> String {
    text.replace('.', r"\.").replace('+', r"\+")
}
