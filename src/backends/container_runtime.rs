// A container runtime is docker/podman/etc.

use std::{
    collections::HashMap,
    collections::HashSet,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use tracing::info;

use crate::{
    backends::podman::PodmanEventStream,
    fakers::{Command, CommandRunner, FdMode},
    models::Image,
};

/// The in-container destination of the `distrobox-init` bind-mount. Distrobox
/// mounts the host's `distrobox-init` at this path and uses it as entrypoint.
pub const ENTRYPOINT_MOUNT_DESTINATION: &str = "/usr/bin/entrypoint";

/// The container runtime distrobox drives under the hood.
///
/// This is a plain value type: each variant carries the path (or bare name,
/// resolved via `PATH`) of the runtime binary, and every operation borrows the
/// [`CommandRunner`] to execute it. Podman's CLI is a drop-in replacement for
/// Docker's, so both variants share the same command implementations and only
/// the invoked binary differs.
///
/// [`ContainerRuntime::Null`] is the null object used where no runtime is
/// available (defaults, tests): every operation fails with a uniform error.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ContainerRuntime {
    #[default]
    Null,
    Docker(PathBuf),
    Podman(PathBuf),
}

#[derive(serde::Deserialize)]
struct InspectOutput {
    #[serde(rename = "Id", default)]
    id: Option<String>,
    #[serde(rename = "Created", default)]
    created: Option<String>,
    #[serde(rename = "State", default)]
    state: Option<InspectState>,
}

#[derive(serde::Deserialize)]
struct InspectMount {
    #[serde(rename = "Source", default)]
    source: Option<String>,
    #[serde(rename = "Destination", default)]
    destination: Option<String>,
}

#[derive(serde::Deserialize, Default)]
struct InspectState {
    #[serde(rename = "StartedAt", default)]
    started_at: Option<String>,
    #[serde(rename = "FinishedAt", default)]
    finished_at: Option<String>,
}

impl ContainerRuntime {
    /// A podman runtime invoked as bare `podman`, resolved via `PATH`.
    pub fn podman() -> Self {
        Self::Podman(PathBuf::from("podman"))
    }

    /// A docker runtime invoked as bare `docker`, resolved via `PATH`.
    pub fn docker() -> Self {
        Self::Docker(PathBuf::from("docker"))
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Docker(_) => "docker",
            Self::Podman(_) => "podman",
        }
    }

    fn binary(&self) -> anyhow::Result<&Path> {
        match self {
            Self::Null => anyhow::bail!("No container runtime available"),
            Self::Docker(path) | Self::Podman(path) => Ok(path),
        }
    }

    pub async fn version(&self, runner: &CommandRunner) -> anyhow::Result<String> {
        let mut cmd = Command::new(self.binary()?);
        cmd.arg("--version");

        let output = runner.output(cmd).await?;

        // A command that spawns but exits non-zero must be treated as "not
        // installed". This matters under Flatpak, where the runtime check runs
        // `flatpak-spawn --host podman --version`: flatpak-spawn itself spawns
        // successfully, so without this check a missing host binary would be
        // mistaken for an available runtime.
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!(
                "version check failed ({}): {}",
                output.status,
                stderr.trim()
            );
        }

        let version = String::from_utf8_lossy(&output.stdout).to_string();
        Ok(version.trim().to_string())
    }

    pub async fn downloaded_images(
        &self,
        runner: &CommandRunner,
    ) -> anyhow::Result<HashSet<String>> {
        let mut cmd = Command::new(self.binary()?);
        cmd.arg("images").arg("--format").arg("json");

        let output = runner.output_string(cmd).await?;
        // Some versions of podman/docker might return empty string if no images?
        if output.trim().is_empty() {
            return Ok(HashSet::new());
        }

        // Handle potential JSON Lines vs JSON Array
        // Try parsing as array first
        let images_vec: Vec<Image> = match serde_json::from_str::<Vec<Image>>(&output) {
            Ok(images) => images,
            Err(_) => {
                // Try parsing as JSON lines
                let mut images = Vec::new();
                for line in output.lines() {
                    if !line.trim().is_empty() {
                        images.push(serde_json::from_str::<Image>(line)?);
                    }
                }
                images
            }
        };

        let names: HashSet<String> = images_vec
            .into_iter()
            .flat_map(|img| img.names.unwrap_or_default())
            .collect();

        Ok(names)
    }

    pub async fn usage(&self, runner: &CommandRunner, container_id: &str) -> anyhow::Result<Usage> {
        let mut cmd = Command::new(self.binary()?);
        cmd.arg("stats");
        cmd.arg("--no-stream");
        cmd.arg("--format");
        cmd.arg("json");
        cmd.arg(container_id);
        cmd.stdout = FdMode::Pipe;
        cmd.stderr = FdMode::Pipe;

        let output = runner.output_string(cmd).await?;
        let usages: Vec<Usage> = serde_json::from_str(&output)?;

        usages
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("No stats found"))
    }

    pub async fn inspect_containers(
        &self,
        runner: &CommandRunner,
        container_ids: &[&str],
    ) -> anyhow::Result<HashMap<String, ContainerInspectInfo>> {
        if container_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let mut cmd = Command::new(self.binary()?);
        cmd.arg("inspect");
        for id in container_ids {
            cmd.arg(id);
        }

        let output = runner.output_string(cmd).await?;
        let inspected: Vec<InspectOutput> = serde_json::from_str(&output)?;

        let mut result = HashMap::new();
        for entry in inspected {
            let full_id = entry
                .id
                .as_deref()
                .map(|s| s.trim_start_matches("sha256:"))
                .unwrap_or("");

            let matched_id = container_ids
                .iter()
                .find(|short_id| full_id.starts_with(*short_id))
                .map(|&s| s.to_string());

            if let Some(id) = matched_id {
                let info = ContainerInspectInfo {
                    created_at: entry.created,
                    started_at: entry.state.as_ref().and_then(|s| {
                        s.started_at
                            .as_deref()
                            .filter(|t| *t != "0001-01-01T00:00:00Z")
                            .map(|t| t.to_string())
                    }),
                    finished_at: entry.state.as_ref().and_then(|s| {
                        s.finished_at
                            .as_deref()
                            .filter(|t| *t != "0001-01-01T00:00:00Z")
                            .map(|t| t.to_string())
                    }),
                };
                result.insert(id, info);
            }
        }

        Ok(result)
    }

    /// Returns the host-side source path of the bind-mount whose destination
    /// is [`ENTRYPOINT_MOUNT_DESTINATION`], or `None` when the container has
    /// no such mount (e.g. it was not created by distrobox).
    pub async fn entrypoint_mount_source(
        &self,
        runner: &CommandRunner,
        container_id: &str,
    ) -> anyhow::Result<Option<String>> {
        let mut cmd = Command::new(self.binary()?);
        cmd.arg("inspect");
        cmd.arg("--format");
        cmd.arg("{{ json .Mounts }}");
        cmd.arg(container_id);

        let output = runner.output(cmd).await?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!(
                "inspect of {} failed ({}): {}",
                container_id,
                output.status,
                stderr.trim()
            );
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let text = stdout.trim();
        if text.is_empty() || text == "null" {
            return Ok(None);
        }

        let mounts: Vec<InspectMount> = serde_json::from_str(text)?;
        Ok(mounts
            .into_iter()
            .find(|m| m.destination.as_deref() == Some(ENTRYPOINT_MOUNT_DESTINATION))
            .and_then(|m| m.source))
    }

    /// Streams runtime events as JSON lines. Only supported with Podman; the
    /// other variants fail so callers degrade to manual refresh.
    pub fn listen_events(
        &self,
        runner: &CommandRunner,
    ) -> Result<PodmanEventStream, std::io::Error> {
        match self {
            Self::Podman(path) => crate::backends::podman::listen_events(runner, path),
            Self::Docker(_) | Self::Null => Err(std::io::Error::other(format!(
                "event streaming is not supported with the {} runtime",
                self.name()
            ))),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ContainerInspectInfo {
    pub created_at: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

impl ContainerInspectInfo {
    pub fn last_used_at(&self) -> Option<&str> {
        // Return the most recent of started_at/finished_at. Naively preferring
        // `finished_at` is wrong for Podman, which does NOT zero out
        // `FinishedAt` for running containers (unlike Docker's
        // `0001-01-01T00:00:00Z` sentinel): a running container would report
        // its stale previous-stop time as "last used", making every running
        // container compare equal and breaking sort-by-last-used. Timestamps
        // from a given runtime share a consistent RFC3339 format and timezone
        // offset, so lexicographic comparison is chronological.
        match (&self.started_at, &self.finished_at) {
            (Some(s), Some(f)) => Some(if s.as_str() >= f.as_str() {
                s.as_str()
            } else {
                f.as_str()
            }),
            (Some(s), None) => Some(s.as_str()),
            (None, Some(f)) => Some(f.as_str()),
            (None, None) => None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Usage {
    #[serde(rename = "mem_usage", alias = "MemUsage")]
    pub mem_usage: String,
    #[serde(rename = "mem_percent", alias = "MemPerc")]
    pub mem_perc: String,
    #[serde(rename = "cpu_percent", alias = "CPU")]
    pub cpu_perc: String,
    #[serde(rename = "net_io", alias = "NetIO")]
    pub net_io: String,
    #[serde(rename = "block_io", alias = "BlockIO")]
    pub block_io: String,
    #[serde(rename = "pids", alias = "PIDs")]
    pub pids: String,
}

/// A container runtime that was detected as available, together with the
/// version string obtained during detection. The version probe already runs as
/// part of detection, so we keep its result here instead of re-fetching it for
/// display.
#[derive(Debug, Clone)]
pub struct DetectedRuntime {
    pub runtime: ContainerRuntime,
    pub version: String,
}

pub async fn get_container_runtime(command_runner: CommandRunner) -> Option<DetectedRuntime> {
    // Prefer Podman when both are available because Podman is rootless by default
    let podman = ContainerRuntime::podman();
    match podman.version(&command_runner).await {
        Ok(version) => Some(DetectedRuntime {
            runtime: podman,
            version,
        }),
        Err(podman_err) => {
            let docker = ContainerRuntime::docker();
            match docker.version(&command_runner).await {
                Ok(version) => Some(DetectedRuntime {
                    runtime: docker,
                    version,
                }),
                Err(docker_err) => {
                    info!(docker = ?docker_err, podman = ?podman_err, "Container runtime check results");
                    None
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fakers::{Command, NullCommandRunnerBuilder};
    use smol::block_on;
    use std::{os::unix::process::ExitStatusExt, process::ExitStatus};

    // A wait status that reports as non-success. On Linux, raw status `1`
    // encodes "terminated" (signal), which `ExitStatus::success()` rejects.
    // This mimics `flatpak-spawn --host <missing-binary>`, which spawns fine
    // but exits non-zero.
    fn failing_status() -> ExitStatus {
        ExitStatusExt::from_raw(1)
    }

    #[test]
    fn test_last_used_at_prefers_more_recent_of_started_and_finished() {
        // Podman does NOT zero out FinishedAt for running containers, so a
        // running container exposes a stale previous-stop time alongside its
        // fresh StartedAt. last_used_at() must return the newer StartedAt,
        // otherwise every running container compares equal and
        // sort-by-last-used silently no-ops.
        let running_podman = ContainerInspectInfo {
            started_at: Some("2026-07-18T17:41:43.123456789+02:00".into()),
            finished_at: Some("2026-07-14T18:51:23.796883590+02:00".into()),
            ..Default::default()
        };
        assert_eq!(
            running_podman.last_used_at(),
            Some("2026-07-18T17:41:43.123456789+02:00"),
            "running container should report its start time, not the stale stop time"
        );

        // Stopped container: FinishedAt is the more recent event.
        let stopped = ContainerInspectInfo {
            started_at: Some("2026-07-10T08:00:00Z".into()),
            finished_at: Some("2026-07-12T09:00:00Z".into()),
            ..Default::default()
        };
        assert_eq!(
            stopped.last_used_at(),
            Some("2026-07-12T09:00:00Z"),
            "stopped container should report its finish time"
        );
    }

    #[test]
    fn test_version_check_rejects_non_zero_exit() {
        // Regression for the Flatpak case: the command spawns (so the runner
        // returns Ok) but exits non-zero. version() must propagate an error
        // rather than return an empty string.
        let runner = NullCommandRunnerBuilder::new()
            .cmd_full_with_status(
                Command::new_with_args("docker", ["--version"]),
                failing_status(),
                || Ok(String::new()),
            )
            .build();

        let result = block_on(ContainerRuntime::docker().version(&runner));

        assert!(
            result.is_err(),
            "version() must error when the command exits non-zero, got: {:?}",
            result
        );
    }

    #[test]
    fn test_null_runtime_fails_every_operation() {
        // The Null variant must never issue commands: it fails uniformly so
        // callers treat it exactly like "no runtime detected".
        let runner = NullCommandRunnerBuilder::new().build();
        let tracker = runner.output_tracker();
        let null = ContainerRuntime::Null;

        assert!(block_on(null.version(&runner)).is_err());
        assert!(block_on(null.downloaded_images(&runner)).is_err());
        assert!(block_on(null.usage(&runner, "ubuntu")).is_err());
        assert!(block_on(null.inspect_containers(&runner, &["ubuntu"])).is_err());
        assert!(block_on(null.entrypoint_mount_source(&runner, "ubuntu")).is_err());
        assert!(null.listen_events(&runner).is_err());
        assert!(
            tracker.items().is_empty(),
            "the null runtime must not run any command"
        );
    }

    #[test]
    fn test_runtime_invokes_its_binary_path() {
        // The variant payload is the binary to invoke, so a custom path must
        // be used verbatim instead of the bare program name.
        let runner = NullCommandRunnerBuilder::new()
            .cmd(
                &["/usr/local/bin/podman", "--version"],
                "podman version 4.9.3",
            )
            .build();

        let podman = ContainerRuntime::Podman(PathBuf::from("/usr/local/bin/podman"));
        let version = block_on(podman.version(&runner)).expect("version should succeed");

        assert_eq!(version, "podman version 4.9.3");
    }

    #[test]
    fn test_get_container_runtime_returns_none_when_no_runtime_available() {
        // Both podman and docker "run" (as flatpak-spawn does) but fail.
        // Neither must be mistaken for an available runtime.
        let runner = NullCommandRunnerBuilder::new()
            .cmd_full_with_status(
                Command::new_with_args("podman", ["--version"]),
                failing_status(),
                || Ok(String::new()),
            )
            .cmd_full_with_status(
                Command::new_with_args("docker", ["--version"]),
                failing_status(),
                || Ok(String::new()),
            )
            .build();

        let runtime = block_on(get_container_runtime(runner));

        assert!(
            runtime.is_none(),
            "expected no runtime when both podman and docker exit non-zero"
        );
    }

    #[test]
    fn test_get_container_runtime_falls_back_to_docker_when_podman_unavailable() {
        // Podman fails to run (non-zero exit) but docker is present: we must
        // fall through to docker instead of falsely detecting podman.
        let runner = NullCommandRunnerBuilder::new()
            .cmd_full_with_status(
                Command::new_with_args("podman", ["--version"]),
                failing_status(),
                || Ok(String::new()),
            )
            .cmd(&["docker", "--version"], "Docker version 24.0.7")
            .build();

        let runtime =
            block_on(get_container_runtime(runner)).expect("a runtime should be detected");

        assert_eq!(runtime.runtime, ContainerRuntime::docker());
    }

    #[test]
    fn test_get_container_runtime_prefers_podman_when_available() {
        let runner = NullCommandRunnerBuilder::new()
            .cmd(&["podman", "--version"], "podman version 4.9.3")
            .build();

        let runtime =
            block_on(get_container_runtime(runner)).expect("a runtime should be detected");

        assert_eq!(runtime.runtime, ContainerRuntime::podman());
    }
}
