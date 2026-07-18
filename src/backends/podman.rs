use std::{
    collections::HashMap,
    path::Path,
    pin::Pin,
    task::{Context, Poll},
};

use futures::Stream;
use serde::Deserialize;

use crate::fakers::{Child, Command, CommandRunner, FdMode};

/// Podman event structure
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PodmanEvent {
    #[allow(dead_code)]
    #[serde(rename = "ID")]
    pub id: Option<String>,
    #[allow(dead_code)]
    pub name: Option<String>,
    pub status: Option<String>,
    #[serde(rename = "Type")]
    pub event_type: Option<String>,
    pub attributes: Option<HashMap<String, String>>,
}

impl PodmanEvent {
    /// Check if this event is for a distrobox container
    pub fn is_distrobox(&self) -> bool {
        self.attributes
            .as_ref()
            .and_then(|attrs| attrs.get("manager"))
            .map(|manager| manager == "distrobox")
            .unwrap_or(false)
    }

    /// Check if this is a container event
    pub fn is_container_event(&self) -> bool {
        self.event_type
            .as_ref()
            .map(|t| t == "container")
            .unwrap_or(false)
    }
}

/// Stream wrapper for podman events
pub struct PodmanEventStream {
    lines: Option<
        futures::io::Lines<futures::io::BufReader<Box<dyn futures::io::AsyncRead + Send + Unpin>>>,
    >,
    _child: Option<Box<dyn Child + Send>>,
}

impl Stream for PodmanEventStream {
    type Item = Result<String, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Some(ref mut lines) = self.lines {
            Pin::new(lines).poll_next(cx)
        } else {
            Poll::Ready(None)
        }
    }
}

/// Spawns `<program> events --format json` and returns a stream of event
/// lines. Callers go through
/// [`ContainerRuntime::listen_events`](crate::backends::container_runtime::ContainerRuntime::listen_events),
/// which supplies the binary path of its `Podman` variant.
pub(crate) fn listen_events(
    runner: &CommandRunner,
    program: &Path,
) -> Result<PodmanEventStream, std::io::Error> {
    use futures::io::{AsyncBufReadExt, BufReader};

    // Create the podman events command
    let mut cmd = Command::new(program);
    cmd.arg("events");
    cmd.arg("--format");
    cmd.arg("json");
    cmd.stdout = FdMode::Pipe;
    cmd.stderr = FdMode::Pipe;

    // Spawn the command
    let mut child = runner.spawn(cmd)?;

    // Get stdout and create a buffered reader
    let stdout = child
        .take_stdout()
        .ok_or_else(|| std::io::Error::other("No stdout available"))?;

    let bufread = BufReader::new(stdout);
    let lines = bufread.lines();

    Ok(PodmanEventStream {
        lines: Some(lines),
        _child: Some(child),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_podman_event_is_distrobox() {
        let mut attrs = HashMap::new();
        attrs.insert("manager".to_string(), "distrobox".to_string());

        let event = PodmanEvent {
            id: Some("abc123".to_string()),
            name: Some("my-container".to_string()),
            status: Some("start".to_string()),
            event_type: Some("container".to_string()),
            attributes: Some(attrs),
        };

        assert!(event.is_distrobox());
    }

    #[test]
    fn test_podman_event_not_distrobox() {
        let mut attrs = HashMap::new();
        attrs.insert("manager".to_string(), "other".to_string());

        let event = PodmanEvent {
            id: Some("abc123".to_string()),
            name: None,
            status: None,
            event_type: None,
            attributes: Some(attrs),
        };

        assert!(!event.is_distrobox());
    }

    #[test]
    fn test_podman_event_no_attributes() {
        let event = PodmanEvent {
            id: None,
            name: None,
            status: None,
            event_type: None,
            attributes: None,
        };

        assert!(!event.is_distrobox());
    }

    #[test]
    fn test_podman_event_is_container_event() {
        let event = PodmanEvent {
            id: None,
            name: None,
            status: None,
            event_type: Some("container".to_string()),
            attributes: None,
        };

        assert!(event.is_container_event());
    }

    #[test]
    fn test_podman_event_not_container_event() {
        let event = PodmanEvent {
            id: None,
            name: None,
            status: None,
            event_type: Some("image".to_string()),
            attributes: None,
        };

        assert!(!event.is_container_event());
    }

    #[test]
    fn test_podman_event_no_type() {
        let event = PodmanEvent {
            id: None,
            name: None,
            status: None,
            event_type: None,
            attributes: None,
        };

        assert!(!event.is_container_event());
    }
}
