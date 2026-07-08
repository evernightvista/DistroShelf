// A container runtime is docker/podman/etc.

use std::{collections::HashMap, collections::HashSet, rc::Rc};

use async_trait::async_trait;
use serde::Deserialize;
use tracing::info;

use super::docker::Docker;

use crate::{backends::podman::Podman, fakers::CommandRunner};

#[async_trait(?Send)]
pub trait ContainerRuntime {
    fn name(&self) -> &'static str;
    async fn version(&self) -> anyhow::Result<String>;
    async fn usage(&self, container_id: &str) -> anyhow::Result<Usage>;
    async fn downloaded_images(&self) -> anyhow::Result<HashSet<String>>;
    async fn inspect_container(&self, container_id: &str) -> anyhow::Result<ContainerInspectInfo>;
    async fn inspect_containers(
        &self,
        container_ids: &[&str],
    ) -> anyhow::Result<HashMap<String, ContainerInspectInfo>>;
}

#[derive(Debug, Clone, Default)]
pub struct ContainerInspectInfo {
    pub created_at: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

impl ContainerInspectInfo {
    pub fn last_used_at(&self) -> Option<&str> {
        self.finished_at
            .as_deref()
            .or(self.started_at.as_deref())
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

pub async fn get_container_runtime(
    command_runner: CommandRunner,
) -> Option<Rc<dyn ContainerRuntime>> {
    // Prefer Podman when both are available because Podman is rootless by default
    let podman = Podman::new(Rc::new(command_runner.clone()));
    if let Err(podman_err) = podman.version().await {
        let docker = Docker::new(Rc::new(command_runner));
        if let Err(docker_err) = docker.version().await {
            info!(docker = ?docker_err, podman = ?podman_err, "Container runtime check results");
            None
        } else {
            Some(Rc::new(docker) as Rc<dyn ContainerRuntime>)
        }
    } else {
        Some(Rc::new(podman) as Rc<dyn ContainerRuntime>)
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

        let docker = Docker::new(Rc::new(runner));
        let result = block_on(docker.version());

        assert!(
            result.is_err(),
            "version() must error when the command exits non-zero, got: {:?}",
            result
        );
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

        assert_eq!(runtime.name(), "docker");
    }

    #[test]
    fn test_get_container_runtime_prefers_podman_when_available() {
        let runner = NullCommandRunnerBuilder::new()
            .cmd(&["podman", "--version"], "podman version 4.9.3")
            .build();

        let runtime =
            block_on(get_container_runtime(runner)).expect("a runtime should be detected");

        assert_eq!(runtime.name(), "podman");
    }
}
