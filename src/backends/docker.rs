use std::{collections::HashMap, collections::HashSet, rc::Rc};

use async_trait::async_trait;

use crate::{
    backends::container_runtime::{
        ContainerInspectInfo, ContainerRuntime, ENTRYPOINT_MOUNT_DESTINATION, Usage,
    },
    fakers::{Command, CommandRunner},
    models::Image,
};

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

pub(crate) struct Docker {
    pub cmd_runner: Rc<CommandRunner>,
}

impl Docker {
    pub fn new(cmd_runner: Rc<CommandRunner>) -> Self {
        Self { cmd_runner }
    }
}

#[async_trait(?Send)]
impl ContainerRuntime for Docker {
    fn name(&self) -> &'static str {
        "docker"
    }
    async fn version(&self) -> anyhow::Result<String> {
        let mut cmd = Command::new("docker");
        cmd.arg("--version");

        let output = self.cmd_runner.output(cmd).await?;

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
    async fn downloaded_images(&self) -> anyhow::Result<HashSet<String>> {
        let mut cmd = Command::new("docker");
        cmd.arg("images").arg("--format").arg("json");

        let output = self.cmd_runner.output_string(cmd).await?;
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

    async fn usage(&self, container_id: &str) -> anyhow::Result<Usage> {
        let mut cmd = Command::new("docker");
        cmd.arg("stats");
        cmd.arg("--no-stream");
        cmd.arg("--format");
        cmd.arg("json");
        cmd.arg(container_id);
        cmd.stdout = crate::fakers::FdMode::Pipe;
        cmd.stderr = crate::fakers::FdMode::Pipe;

        let output = self.cmd_runner.output_string(cmd).await?;
        let usages: Vec<Usage> = serde_json::from_str(&output)?;

        usages
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("No stats found"))
    }

    async fn inspect_container(&self, container_id: &str) -> anyhow::Result<ContainerInspectInfo> {
        let ids = [container_id];
        self.inspect_containers(&ids)
            .await?
            .into_values()
            .next()
            .ok_or_else(|| anyhow::anyhow!("No inspect result for container {container_id}"))
    }

    async fn inspect_containers(
        &self,
        container_ids: &[&str],
    ) -> anyhow::Result<HashMap<String, ContainerInspectInfo>> {
        if container_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let mut cmd = Command::new("docker");
        cmd.arg("inspect");
        for id in container_ids {
            cmd.arg(id);
        }

        let output = self.cmd_runner.output_string(cmd).await?;
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

    async fn entrypoint_mount_source(&self, container_id: &str) -> anyhow::Result<Option<String>> {
        let mut cmd = Command::new("docker");
        cmd.arg("inspect");
        cmd.arg("--format");
        cmd.arg("{{ json .Mounts }}");
        cmd.arg(container_id);

        let output = self.cmd_runner.output(cmd).await?;
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
}
