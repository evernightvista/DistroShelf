use crate::fakers::CommandRunner;

use super::Error;
use super::command::CmdFactory;

/// Parse a distrobox version string from command output.
///
/// Supports both the v1 format (`distrobox: 1.8.2.5`, printed by
/// `distrobox version` / `--version` in v1) and the v2 format
/// (`distrobox version 2.0.0-rc.4` or `distrobox version dev`, printed by
/// `distrobox --version` in v2). Returns `None` for unparseable output.
fn parse_distrobox_version(output: &str) -> Option<String> {
    let text = output.trim();
    if text.is_empty() {
        return None;
    }
    if let Some(version) = text
        .split(':')
        .nth(1)
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        return Some(version.to_string());
    }
    let mut words = text.split_whitespace();
    if words.next()? == "distrobox"
        && words.next()? == "version"
        && let Some(version) = words.next().map(str::trim).filter(|v| !v.is_empty())
    {
        return Some(version.to_string());
    }
    None
}

/// Run a single version probe and parse its output. The failure is reported
/// with the same fidelity as the rest of the backend: spawn errors become
/// [`Error::Spawn`], non-zero exits [`Error::CommandFailed`], and unparseable
/// output [`Error::ParseOutput`] (carrying the raw output).
async fn probe_version(
    command_runner: &CommandRunner,
    cmd_factory: &CmdFactory,
    flag: &str,
) -> Result<String, Error> {
    let mut cmd = cmd_factory();
    cmd.arg(flag);
    let program = cmd.program.to_string_lossy().to_string();
    let command_str = format!("{:?} {:?}", program, cmd.args);

    let output = command_runner
        .output(cmd)
        .await
        .map_err(|source| Error::Spawn {
            source,
            command: command_str.clone(),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let exit_code = output.status.code();
        return Err(Error::CommandFailed {
            exit_code,
            command: command_str,
            stderr,
        });
    }

    let raw = String::from_utf8_lossy(&output.stdout).into_owned();
    match parse_distrobox_version(&raw) {
        Some(version) => Ok(version),
        None => Err(Error::ParseOutput(raw)),
    }
}

/// Fetch the installed distrobox version.
///
/// Tries `distrobox version` (v1) first and falls back to `distrobox --version`
/// (v2, where the `version` subcommand was removed). A probe failing (spawn
/// error, non-zero exit, unparseable output) never loses the fallback; when
/// both probes fail, the second probe's error is returned so callers can
/// diagnose the cause (e.g. a missing bundled binary surfaces as
/// [`Error::Spawn`] instead of a generic parse error).
pub async fn fetch_distrobox_version(
    command_runner: &CommandRunner,
    cmd_factory: &CmdFactory,
) -> Result<String, Error> {
    match probe_version(command_runner, cmd_factory, "version").await {
        Ok(version) => Ok(version),
        Err(_) => probe_version(command_runner, cmd_factory, "--version").await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::distrobox::command::default_cmd_factory;
    use crate::fakers::{Command, NullCommandRunnerBuilder};
    use smol::block_on;
    use std::io;
    use std::os::unix::process::ExitStatusExt;

    #[test]
    fn parses_v1_colon_format() {
        assert_eq!(
            parse_distrobox_version("distrobox: 1.8.2.5"),
            Some("1.8.2.5".to_string())
        );
        assert_eq!(
            parse_distrobox_version("distrobox: 1.8.2.5\n"),
            Some("1.8.2.5".to_string())
        );
    }

    #[test]
    fn parses_v2_flag_format() {
        assert_eq!(
            parse_distrobox_version("distrobox version 2.0.0-rc.4"),
            Some("2.0.0-rc.4".to_string())
        );
        assert_eq!(
            parse_distrobox_version("distrobox version dev"),
            Some("dev".to_string())
        );
    }

    #[test]
    fn rejects_garbage_and_empty_output() {
        assert_eq!(parse_distrobox_version(""), None);
        assert_eq!(parse_distrobox_version("   \n "), None);
        assert_eq!(parse_distrobox_version("total garbage here"), None);
        assert_eq!(parse_distrobox_version("distrobox"), None);
        assert_eq!(parse_distrobox_version("distrobox version"), None);
    }

    #[test]
    fn colon_format_matches_v1_parser_behavior() {
        assert_eq!(parse_distrobox_version("foo: bar"), Some("bar".to_string()));
    }

    #[test]
    fn fetch_uses_v1_subcommand_first() {
        let runner = NullCommandRunnerBuilder::new()
            .cmd(&["distrobox", "version"], "distrobox: 1.8.2.5")
            .build();
        assert_eq!(
            block_on(fetch_distrobox_version(&runner, &default_cmd_factory())).unwrap(),
            "1.8.2.5".to_string()
        );
    }

    #[test]
    fn fetch_falls_back_to_flag_when_subcommand_fails() {
        let runner = NullCommandRunnerBuilder::new()
            .cmd_full_with_status(
                Command::new_with_args("distrobox", ["version"]),
                ExitStatusExt::from_raw(3),
                || Ok("No help topic for 'version'".to_string()),
            )
            .cmd(&["distrobox", "--version"], "distrobox version 2.0.0-rc.4")
            .build();
        assert_eq!(
            block_on(fetch_distrobox_version(&runner, &default_cmd_factory())).unwrap(),
            "2.0.0-rc.4".to_string()
        );
    }

    #[test]
    fn fetch_falls_back_to_flag_when_subcommand_fails_to_spawn() {
        let runner = NullCommandRunnerBuilder::new()
            .cmd_full(Command::new_with_args("distrobox", ["version"]), || {
                Err(io::Error::from_raw_os_error(2))
            })
            .cmd(&["distrobox", "--version"], "distrobox version 2.0.0-rc.4")
            .build();
        assert_eq!(
            block_on(fetch_distrobox_version(&runner, &default_cmd_factory())).unwrap(),
            "2.0.0-rc.4".to_string()
        );
    }

    #[test]
    fn fetch_falls_back_when_subcommand_output_is_unparseable() {
        let runner = NullCommandRunnerBuilder::new()
            .cmd(&["distrobox", "version"], "not a version at all")
            .cmd(&["distrobox", "--version"], "distrobox version 2.0.0-rc.4")
            .build();
        assert_eq!(
            block_on(fetch_distrobox_version(&runner, &default_cmd_factory())).unwrap(),
            "2.0.0-rc.4".to_string()
        );
    }

    #[test]
    fn fetch_returns_command_failed_error_for_nonzero_exits() {
        let runner = NullCommandRunnerBuilder::new()
            .cmd_full_with_status(
                Command::new_with_args("distrobox", ["version"]),
                ExitStatusExt::from_raw(3),
                || Ok("No help topic for 'version'".to_string()),
            )
            .cmd_full_with_status(
                Command::new_with_args("distrobox", ["--version"]),
                ExitStatusExt::from_raw(3),
                || Ok("No help topic for 'version'".to_string()),
            )
            .build();
        assert!(matches!(
            block_on(fetch_distrobox_version(&runner, &default_cmd_factory())),
            Err(Error::CommandFailed { .. })
        ));
    }

    #[test]
    fn fetch_returns_parse_output_error_with_raw_output() {
        let runner = NullCommandRunnerBuilder::new()
            .cmd(&["distrobox", "version"], "garbage")
            .cmd(&["distrobox", "--version"], "also garbage")
            .build();
        assert!(matches!(
            block_on(fetch_distrobox_version(&runner, &default_cmd_factory())),
            Err(Error::ParseOutput(raw)) if raw.contains("also garbage")
        ));
    }

    #[test]
    fn fetch_returns_spawn_error_when_both_probes_fail_to_spawn() {
        let runner = NullCommandRunnerBuilder::new()
            .cmd_full(Command::new_with_args("distrobox", ["version"]), || {
                Err(io::Error::from_raw_os_error(2))
            })
            .cmd_full(Command::new_with_args("distrobox", ["--version"]), || {
                Err(io::Error::from_raw_os_error(2))
            })
            .build();
        assert!(matches!(
            block_on(fetch_distrobox_version(&runner, &default_cmd_factory())),
            Err(Error::Spawn { .. })
        ));
    }

    #[test]
    fn fetch_errors_with_empty_runner() {
        // Unregistered commands succeed with empty stdout, so both probes hit
        // the unparseable-output path and the last (--version) error surfaces.
        let runner = NullCommandRunnerBuilder::new().build();
        assert!(matches!(
            block_on(fetch_distrobox_version(&runner, &default_cmd_factory())),
            Err(Error::ParseOutput(_))
        ));
    }
}
