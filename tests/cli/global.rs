//! Behavior shared by every subcommand: the top-level help, and the global
//! `--log-level` and `--enable-colors` flags. These flags act the same
//! whichever subcommand they're given to, so each is covered once here.

use insta_cmd::assert_cmd_snapshot;

use super::{TODO_API_YAML, Workspace, with_violation};

/// Run `body` with log lines' timestamps, which change every run, redacted.
fn without_timestamps(body: impl FnOnce()) {
    let mut settings = insta::Settings::clone_current();
    settings.add_filter(
        r"(?m)^.+? (TRACE|DEBUG|INFO|WARN|ERROR) ",
        "[TIMESTAMP] $1 ",
    );
    settings.bind(body);
}

/// Run `body` with ANSI escape characters shown as `␛`, so that colored
/// output is legible in a snapshot.
fn with_visible_escapes(body: impl FnOnce()) {
    let mut settings = insta::Settings::clone_current();
    settings.add_filter("\x1b", "␛");
    settings.bind(body);
}

#[test]
fn no_subcommand_prints_the_help() {
    assert_cmd_snapshot!(Workspace::new().slcr(&[]));
}

#[test]
fn an_unknown_subcommand_is_rejected() {
    assert_cmd_snapshot!(Workspace::new().slcr(&["frobnicate"]));
}

#[test]
fn stubbed_subcommands_are_not_implemented() {
    let workspace = Workspace::new();
    assert_cmd_snapshot!("slice", workspace.slcr(&["slice"]));
    assert_cmd_snapshot!("eval", workspace.slcr(&["eval"]));
}

#[test]
fn log_level_shows_lower_levels_on_stderr() {
    let workspace = Workspace::new();
    workspace.file("SPEC.slcr.yml", TODO_API_YAML);
    without_timestamps(|| {
        assert_cmd_snapshot!(workspace.slcr(&["--log-level", "debug", "render"]));
    });
}

#[test]
fn log_level_can_come_from_the_environment() {
    let workspace = Workspace::new();
    let mut command = workspace.slcr(&["init", "todo-api.yaml"]);
    command.env("LOG_LEVEL", "DEBUG");
    without_timestamps(|| {
        assert_cmd_snapshot!(command);
    });
}

#[test]
fn an_unknown_log_level_is_rejected() {
    assert_cmd_snapshot!(Workspace::new().slcr(&["--log-level", "loud", "version"]));
}

#[test]
fn colors_can_be_forced_on() {
    let workspace = Workspace::new();
    workspace.file("broken.yaml", with_violation());
    with_visible_escapes(|| {
        assert_cmd_snapshot!(workspace.slcr(&[
            "check",
            "broken.yaml",
            "--enable-colors",
            "always"
        ]));
    });
}

#[test]
fn colors_can_be_forced_off_whatever_the_environment_says() {
    let workspace = Workspace::new();
    workspace.file("broken.yaml", with_violation());
    let mut command = workspace.slcr(&["--enable-colors", "never", "check", "broken.yaml"]);
    command.env("CLICOLOR_FORCE", "1");
    with_visible_escapes(|| {
        assert_cmd_snapshot!(command);
    });
}

#[test]
fn an_unknown_color_preference_is_rejected() {
    assert_cmd_snapshot!(Workspace::new().slcr(&["--enable-colors", "sometimes", "version"]));
}
