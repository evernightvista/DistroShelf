//! Migration of containers whose baked-in `distrobox-init` path became stale.
//!
//! Distrobox bind-mounts the host's `distrobox-init` into every container it
//! creates, and the absolute host path is baked into the container's config.
//! When that path disappears (an old version-specific bundle directory such as
//! `distrobox-1.8.2.1/` was removed, or the user switched between the host and
//! the bundled distrobox), the container can no longer start.
//!
//! The fix is non-destructive: place a symlink at the stale path pointing to
//! the current `distrobox-init`. The container runtime follows regular
//! symlinks when resolving bind-mount sources at start time, so the container
//! starts again with the up-to-date init script and its filesystem untouched.
//!
//! See docs/distrobox-init-migration.md for the full design rationale.
//!
//! All filesystem operations go through [`CommandRunner`] so they act on the
//! host even when DistroShelf runs inside a Flatpak sandbox.

use std::path::{Path, PathBuf};

use crate::backends::container_runtime::ContainerRuntime;
use crate::distrobox_downloader::path_exists;
use crate::fakers::{Command, CommandRunner};

/// A container whose `distrobox-init` bind-mount source no longer resolves.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StaleContainer {
    pub name: String,
    /// The host path the container expects `distrobox-init` at.
    pub stale_init_path: PathBuf,
    /// Whether the container was running when the check ran. Running
    /// containers are reported but not migrated (see the docs: creating the
    /// symlink while the container is starting is racy).
    pub running: bool,
}

/// Resolves the `distrobox-init` location for the given `distrobox`
/// executable path. Upstream distrobox provisions its scripts in the
/// directory of the executable (`hostDir()`), so `distrobox-init` is always a
/// sibling of the `distrobox` binary.
///
/// Returns `None` for bare program names (e.g. `"distrobox"` resolved via
/// `PATH`) because they carry no directory information.
pub fn current_init_path(distrobox_exe_path: &str) -> Option<PathBuf> {
    let dir = Path::new(distrobox_exe_path).parent()?;
    if dir.as_os_str().is_empty() {
        return None;
    }
    Some(dir.join("distrobox-init"))
}

/// Inspects the given containers (`(name, running)` pairs) and returns those
/// whose entrypoint bind-mount source differs from `current_init` *and* no
/// longer exists on the host.
///
/// Containers whose stale path still resolves are skipped: they keep working
/// with the init script they were created with (e.g. the host distrobox is
/// still installed, or a symlink from a previous migration is in place),
/// which also makes re-running this check on migrated containers a no-op.
pub async fn find_stale_containers(
    runner: &CommandRunner,
    runtime: &dyn ContainerRuntime,
    containers: &[(String, bool)],
    current_init: &Path,
) -> Vec<StaleContainer> {
    let mut stale = Vec::new();
    for (name, running) in containers {
        let source = match runtime.entrypoint_mount_source(name).await {
            Ok(Some(source)) => PathBuf::from(source),
            Ok(None) => {
                tracing::warn!(
                    container = %name,
                    "No entrypoint bind-mount found; skipping stale-init check"
                );
                continue;
            }
            Err(e) => {
                tracing::warn!(
                    container = %name,
                    error = %e,
                    "Failed to inspect container; skipping stale-init check"
                );
                continue;
            }
        };

        if source == current_init {
            // Paths match: not a migration problem. A missing file here means
            // the init script is absent from its canonical location, which
            // requires re-provisioning (re-downloading the bundle), not a
            // symlink pointing at itself.
            if !path_exists(runner, &source).await {
                tracing::warn!(
                    path = %source.display(),
                    "distrobox-init missing at its canonical location; the bundle needs re-provisioning"
                );
            }
            continue;
        }

        if path_exists(runner, &source).await {
            continue;
        }

        stale.push(StaleContainer {
            name: name.clone(),
            stale_init_path: source,
            running: *running,
        });
    }
    stale
}

/// Repairs a stale init path by symlinking it to the current
/// `distrobox-init`. Only call this for paths reported by
/// [`find_stale_containers`], which guarantees the path does not resolve to
/// an existing file.
pub async fn migrate_stale_path(
    runner: &CommandRunner,
    stale_init_path: &Path,
    current_init: &Path,
) -> anyhow::Result<()> {
    let parent = stale_init_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Stale init path {} has no parent directory",
                stale_init_path.display()
            )
        })?;

    let mut mkdir_cmd = Command::new("mkdir");
    mkdir_cmd.arg("-p").arg(parent);
    let output = runner.output(mkdir_cmd).await?;
    if !output.status.success() {
        anyhow::bail!(
            "Failed to create directory {}: {}. If this is a system directory, \
             install distrobox on the host to restore the path instead.",
            parent.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    // -sfn replaces a possibly broken leftover symlink at the stale path.
    // find_stale_containers only reports paths that do not resolve, so no
    // real file can be overwritten here.
    let mut ln_cmd = Command::new("ln");
    ln_cmd.arg("-sfn").arg(current_init).arg(stale_init_path);
    let output = runner.output(ln_cmd).await?;
    if !output.status.success() {
        anyhow::bail!(
            "Failed to create symlink at {}: {}",
            stale_init_path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    if !path_exists(runner, stale_init_path).await {
        anyhow::bail!(
            "Symlink at {} does not resolve after migration; is {} missing?",
            stale_init_path.display(),
            current_init.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::podman::Podman;
    use crate::fakers::{CommandRunnerEvent, NullCommandRunnerBuilder};
    use smol::block_on;
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;
    use std::rc::Rc;

    const CURRENT_INIT: &str =
        "/home/user/.local/share/distroshelf/distrobox-bundled/distrobox-init";
    const STALE_INIT: &str = "/home/user/.local/share/distroshelf/distrobox-1.8.2.1/distrobox-init";

    fn failing_status() -> ExitStatus {
        ExitStatusExt::from_raw(1)
    }

    fn inspect_cmd(container: &str) -> Command {
        Command::new_with_args(
            "podman",
            ["inspect", "--format", "{{ json .Mounts }}", container],
        )
    }

    fn mounts_json(source: &str) -> String {
        format!(
            r#"[{{"Type":"bind","Source":"{}","Destination":"/usr/bin/entrypoint","Mode":"ro","RW":false,"Propagation":"rprivate"}}]"#,
            source
        )
    }

    #[test]
    fn test_current_init_path() {
        assert_eq!(
            current_init_path("/home/user/.local/share/distroshelf/distrobox-bundled/distrobox"),
            Some(PathBuf::from(CURRENT_INIT))
        );
        assert_eq!(
            current_init_path("/usr/bin/distrobox"),
            Some(PathBuf::from("/usr/bin/distrobox-init"))
        );
        assert_eq!(current_init_path("distrobox"), None);
        assert_eq!(current_init_path(""), None);
    }

    #[test]
    fn test_find_stale_containers_flags_missing_source() {
        let json = mounts_json(STALE_INIT);
        let runner = NullCommandRunnerBuilder::new()
            .cmd_full(inspect_cmd("ubuntu"), move || Ok(json.clone()))
            .cmd_full_with_status(
                Command::new_with_args("test", ["-e", STALE_INIT]),
                failing_status(),
                || Ok(String::new()),
            )
            .build();
        let podman = Podman::new(Rc::new(runner.clone()));

        let stale = block_on(find_stale_containers(
            &runner,
            &podman,
            &[("ubuntu".to_string(), false)],
            Path::new(CURRENT_INIT),
        ));

        assert_eq!(
            stale,
            vec![StaleContainer {
                name: "ubuntu".to_string(),
                stale_init_path: PathBuf::from(STALE_INIT),
                running: false,
            }]
        );
    }

    #[test]
    fn test_find_stale_containers_skips_matching_source() {
        let json = mounts_json(CURRENT_INIT);
        let runner = NullCommandRunnerBuilder::new()
            .cmd_full(inspect_cmd("ubuntu"), move || Ok(json.clone()))
            .build();
        let podman = Podman::new(Rc::new(runner.clone()));

        let stale = block_on(find_stale_containers(
            &runner,
            &podman,
            &[("ubuntu".to_string(), false)],
            Path::new(CURRENT_INIT),
        ));

        assert!(stale.is_empty());
    }

    #[test]
    fn test_find_stale_containers_skips_matching_source_even_when_missing() {
        // The mount matches the canonical location but the file is gone:
        // that's a provisioning failure (the bundle must be re-downloaded),
        // not a migration problem — a self-referencing symlink would be
        // useless. The container must not be flagged.
        let json = mounts_json(CURRENT_INIT);
        let runner = NullCommandRunnerBuilder::new()
            .cmd_full(inspect_cmd("ubuntu"), move || Ok(json.clone()))
            .cmd_full_with_status(
                Command::new_with_args("test", ["-e", CURRENT_INIT]),
                failing_status(),
                || Ok(String::new()),
            )
            .build();
        let podman = Podman::new(Rc::new(runner.clone()));

        let stale = block_on(find_stale_containers(
            &runner,
            &podman,
            &[("ubuntu".to_string(), false)],
            Path::new(CURRENT_INIT),
        ));

        assert!(stale.is_empty());
    }

    #[test]
    fn test_find_stale_containers_skips_existing_source() {
        // The mount points elsewhere, but the path still exists (e.g. host
        // distrobox still installed, or a symlink left by a previous
        // migration): the container works, nothing to do.
        let json = mounts_json("/usr/bin/distrobox-init");
        let runner = NullCommandRunnerBuilder::new()
            .cmd_full(inspect_cmd("ubuntu"), move || Ok(json.clone()))
            .cmd(
                &["test", "-e", "/usr/bin/distrobox-init"],
                "", // exists: exit status 0
            )
            .build();
        let podman = Podman::new(Rc::new(runner.clone()));

        let stale = block_on(find_stale_containers(
            &runner,
            &podman,
            &[("ubuntu".to_string(), false)],
            Path::new(CURRENT_INIT),
        ));

        assert!(stale.is_empty());
    }

    #[test]
    fn test_find_stale_containers_skips_containers_without_entrypoint_mount() {
        let runner = NullCommandRunnerBuilder::new()
            .cmd_full(inspect_cmd("plain"), || Ok("null".to_string()))
            .cmd_full(inspect_cmd("other"), || {
                Ok(r#"[{"Type":"bind","Source":"/tmp","Destination":"/tmp"}]"#.to_string())
            })
            .build();
        let podman = Podman::new(Rc::new(runner.clone()));

        let stale = block_on(find_stale_containers(
            &runner,
            &podman,
            &[("plain".to_string(), false), ("other".to_string(), false)],
            Path::new(CURRENT_INIT),
        ));

        assert!(stale.is_empty());
    }

    #[test]
    fn test_find_stale_containers_skips_failed_inspect() {
        let runner = NullCommandRunnerBuilder::new()
            .cmd_full_with_status(
                inspect_cmd("broken"),
                failing_status(),
                || Ok(String::new()),
            )
            .build();
        let podman = Podman::new(Rc::new(runner.clone()));

        let stale = block_on(find_stale_containers(
            &runner,
            &podman,
            &[("broken".to_string(), false)],
            Path::new(CURRENT_INIT),
        ));

        assert!(stale.is_empty());
    }

    #[test]
    fn test_find_stale_containers_reports_running_state() {
        let json = mounts_json(STALE_INIT);
        let runner = NullCommandRunnerBuilder::new()
            .cmd_full(inspect_cmd("ubuntu"), move || Ok(json.clone()))
            .cmd_full_with_status(
                Command::new_with_args("test", ["-e", STALE_INIT]),
                failing_status(),
                || Ok(String::new()),
            )
            .build();
        let podman = Podman::new(Rc::new(runner.clone()));

        let stale = block_on(find_stale_containers(
            &runner,
            &podman,
            &[("ubuntu".to_string(), true)],
            Path::new(CURRENT_INIT),
        ));

        assert_eq!(stale.len(), 1);
        assert!(stale[0].running);
    }

    #[test]
    fn test_migrate_stale_path_creates_dir_and_symlink() {
        let runner = NullCommandRunnerBuilder::new().build();
        let tracker = runner.output_tracker();

        block_on(migrate_stale_path(
            &runner,
            Path::new(STALE_INIT),
            Path::new(CURRENT_INIT),
        ))
        .unwrap();

        let commands: Vec<String> = tracker
            .items()
            .iter()
            .filter_map(|e| match e {
                CommandRunnerEvent::Started(_, cmd) => Some(cmd.to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(
            commands,
            vec![
                format!("mkdir -p /home/user/.local/share/distroshelf/distrobox-1.8.2.1"),
                format!("ln -sfn {} {}", CURRENT_INIT, STALE_INIT),
                format!("test -e {}", STALE_INIT),
            ]
        );
    }

    #[test]
    fn test_migrate_stale_path_fails_on_unwritable_dir() {
        let runner = NullCommandRunnerBuilder::new()
            .cmd_full_with_status(
                Command::new_with_args("mkdir", ["-p", "/usr/bin"]),
                failing_status(),
                || Ok(String::new()),
            )
            .build();

        let result = block_on(migrate_stale_path(
            &runner,
            Path::new("/usr/bin/distrobox-init"),
            Path::new(CURRENT_INIT),
        ));

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("install distrobox on the host")
        );
    }

    #[test]
    fn test_migrate_stale_path_fails_when_symlink_does_not_resolve() {
        let runner = NullCommandRunnerBuilder::new()
            .cmd_full_with_status(
                Command::new_with_args("test", ["-e", STALE_INIT]),
                failing_status(),
                || Ok(String::new()),
            )
            .build();

        let result = block_on(migrate_stale_path(
            &runner,
            Path::new(STALE_INIT),
            Path::new(CURRENT_INIT),
        ));

        assert!(result.is_err());
    }
}
