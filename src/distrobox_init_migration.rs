//! Migration of containers whose baked-in `distrobox-init` path became stale.
//!
//! Distrobox bind-mounts three host scripts into every container it creates
//! (see upstream `pkg/containermanager/providers/podman.go` and `docker.go`):
//!
//! | Host script           | In-container destination        |
//! |-----------------------|---------------------------------|
//! | `distrobox-init`      | `/usr/bin/entrypoint:ro`        |
//! | `distrobox-export`    | `/usr/bin/distrobox-export:ro`  |
//! | `distrobox-host-exec` | `/usr/bin/distrobox-host-exec:ro`|
//!
//! All three sources live in the same directory — the dir of the `distrobox`
//! binary itself (`hostDir()` in `internal/inside-distrobox/scripts.go`) — and
//! the absolute host paths are baked into the container's config at creation
//! time. When that directory disappears (an old version-specific bundle such as
//! `distrobox-1.8.2.1/` was removed, or the user switched between the host and
//! the bundled distrobox), all three sources vanish together and the container
//! can no longer start. Repairing `distrobox-init` alone leaves the container
//! unable to invoke `distrobox-export` or `distrobox-host-exec` (e.g. via
//! `distrobox-host-exec` from inside the container).
//!
//! The fix is non-destructive: place a symlink at each stale path pointing to
//! the current script. The container runtime follows regular symlinks when
//! resolving bind-mount sources at start time, so the container starts again
//! with the up-to-date scripts and its filesystem untouched.
//!
//! See docs/distrobox-init-migration.md for the full design rationale.
//!
//! All filesystem operations go through [`CommandRunner`] so they act on the
//! host even when DistroShelf runs inside a Flatpak sandbox.

use std::path::{Path, PathBuf};

use crate::backends::container_runtime::ContainerRuntime;
use crate::distrobox_downloader::path_exists;
use crate::fakers::{Command, CommandRunner};

/// Filename of the entrypoint script. Distrobox bind-mounts `distrobox-init`
/// at `/usr/bin/entrypoint` and uses it as the container's entrypoint; this
/// is the canonical path [`find_stale_containers`] detects staleness on.
///
/// Listed here (rather than inlined) so the entrypoint handling in
/// [`migrate_stale_path`] can refer to it by name when filtering siblings.
const ENTRYPOINT_SCRIPT: &str = "distrobox-init";

/// Prefix shared by every distrobox script that lives in `hostDir()`.
/// `distrobox-init`, `distrobox-export`, and `distrobox-host-exec` all start
/// with this prefix; the `distrobox` binary itself does not (no trailing
/// hyphen), so the prefix doubles as a safe glob for "all distrobox scripts
/// in the directory" without picking up the binary or unrelated files like
/// the bundled install's `VERSION` marker.
const DISTROBOX_SCRIPT_PREFIX: &str = "distrobox-";

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

/// Lists the distrobox sibling scripts present in `current_dir`, i.e. every
/// entry matching `distrobox-*` except the entrypoint itself (which
/// [`migrate_stale_path`] handles separately). The directory listing is the
/// existence proof, so callers can link every returned entry without an
/// extra `test -e`.
///
/// Returns an empty vector if the directory cannot be listed (e.g. the
/// bundle is missing entirely, in which case the caller's entrypoint
/// post-condition will surface the failure). Other `ls` failure modes that
/// do *not* imply the entrypoint is also unreadable (e.g. a transient
/// filesystem error, or a directory with execute-but-not-read permission)
/// are similarly swallowed: the migration will succeed with no siblings
/// linked. This is a deliberate trade-off — re-running the migration after
/// fixing the underlying permission issue repairs the missing siblings.
async fn list_sibling_scripts(runner: &CommandRunner, current_dir: &Path) -> Vec<String> {
    let mut ls_cmd = Command::new("ls");
    ls_cmd.arg("-1").arg(current_dir);
    let output = match runner.output(ls_cmd).await {
        Ok(o) if o.status.success() => o,
        _ => {
            tracing::warn!(
                dir = %current_dir.display(),
                "Failed to list current distrobox directory; skipping sibling scripts"
            );
            return Vec::new();
        }
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let mut siblings: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|name| name.starts_with(DISTROBOX_SCRIPT_PREFIX))
        .filter(|name| *name != ENTRYPOINT_SCRIPT)
        .map(str::to_string)
        .collect();
    // Sort for deterministic command order (test assertions, task log output).
    siblings.sort();
    siblings
}

/// Repairs a stale init path by symlinking it — and its sibling distrobox
/// scripts (`distrobox-export`, `distrobox-host-exec`) — to the current
/// `distrobox-init` directory. Only call this for paths reported by
/// [`find_stale_containers`], which guarantees the path does not resolve to
/// an existing file.
///
/// # Entrypoint vs siblings
///
/// The entrypoint (`distrobox-init`) is **mandatory**: it is always symlinked
/// first, and the function bails if the resulting symlink does not resolve
/// (the canonical signal that the current bundle is broken).
///
/// Sibling scripts are discovered at runtime by listing `current_dir` for
/// `distrobox-*` entries, so any new script upstream adds is picked up
/// automatically. They share the entrypoint's parent directory (`hostDir()`),
/// so if the entrypoint source is stale they are stale too; we repair them in
/// the same pass to keep the container fully functional (otherwise
/// `distrobox-host-exec` / `distrobox-export` would fail inside the container
/// even though the entrypoint resolves).
pub async fn migrate_stale_path(
    runner: &CommandRunner,
    stale_init_path: &Path,
    current_init: &Path,
) -> anyhow::Result<()> {
    let stale_dir = stale_init_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Stale init path {} has no parent directory",
                stale_init_path.display()
            )
        })?;
    let current_dir = current_init
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Current init path {} has no parent directory",
                current_init.display()
            )
        })?;

    let mut mkdir_cmd = Command::new("mkdir");
    mkdir_cmd.arg("-p").arg(stale_dir);
    let output = runner.output(mkdir_cmd).await?;
    if !output.status.success() {
        anyhow::bail!(
            "Failed to create directory {}: {}. If this is a system directory, \
             install distrobox on the host to restore the path instead.",
            stale_dir.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    // Entrypoint (`distrobox-init`): mandatory. Symlink it first; the
    // post-condition check below catches a missing current init with a
    // clearer message than a pre-check would.
    //
    // -sfn replaces a possibly broken leftover symlink at the stale path.
    // find_stale_containers only reports entrypoint paths that do not
    // resolve, so no real file can be overwritten here.
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

    // Sibling scripts: discovered by listing current_dir for `distrobox-*`.
    // The listing is the existence proof, so each entry can be linked
    // directly. `ln -sfn` will overwrite any leftover file at the stale
    // sibling path — this is intended: the sibling should track the current
    // distrobox version, and find_stale_containers does not pre-check
    // sibling paths.
    for script in list_sibling_scripts(runner, current_dir).await {
        let current_script = current_dir.join(&script);
        let stale_script = stale_dir.join(&script);

        let mut ln_cmd = Command::new("ln");
        ln_cmd.arg("-sfn").arg(&current_script).arg(&stale_script);
        let output = runner.output(ln_cmd).await?;
        if !output.status.success() {
            anyhow::bail!(
                "Failed to create symlink at {}: {}",
                stale_script.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
    }

    // The entrypoint is the canonical signal: if it does not resolve after
    // migration, the bundle is broken and the container still won't start.
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
    const CURRENT_DIR: &str = "/home/user/.local/share/distroshelf/distrobox-bundled";
    const STALE_DIR: &str = "/home/user/.local/share/distroshelf/distrobox-1.8.2.1";

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

    /// Builds the `ls -1 <current_dir>` command that [`list_sibling_scripts`]
    /// issues to enumerate the current bundle directory.
    fn ls_cmd() -> Command {
        Command::new_with_args("ls", ["-1", CURRENT_DIR])
    }

    /// Realistic bundle dir contents: the `distrobox` binary (no `distrobox-`
    /// prefix), the three scripts, and the `VERSION` marker. Only the two
    /// non-entrypoint scripts (`distrobox-export`, `distrobox-host-exec`)
    /// should be linked as siblings.
    const REALISTIC_BUNDLE_LS: &str =
        "distrobox\ndistrobox-init\ndistrobox-export\ndistrobox-host-exec\nVERSION\n";

    #[test]
    fn test_migrate_stale_path_creates_dir_and_symlink() {
        let runner = NullCommandRunnerBuilder::new()
            .cmd_full(ls_cmd(), move || Ok(REALISTIC_BUNDLE_LS.to_string()))
            .build();
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
        // mkdir, entrypoint is linked unconditionally, then the bundle dir is
        // listed and every `distrobox-*` sibling (except the entrypoint) is
        // linked. The `distrobox` binary and `VERSION` are filtered out by
        // the prefix. The final `test -e <stale_init>` is the canonical
        // post-condition check on the entrypoint.
        assert_eq!(
            commands,
            vec![
                format!("mkdir -p {}", STALE_DIR),
                format!("ln -sfn {} {}", CURRENT_INIT, STALE_INIT),
                format!("ls -1 {}", CURRENT_DIR),
                format!(
                    "ln -sfn {}/distrobox-export {}/distrobox-export",
                    CURRENT_DIR, STALE_DIR
                ),
                format!(
                    "ln -sfn {}/distrobox-host-exec {}/distrobox-host-exec",
                    CURRENT_DIR, STALE_DIR
                ),
                format!("test -e {}", STALE_INIT),
            ]
        );
    }

    #[test]
    fn test_migrate_stale_path_links_no_siblings_when_dir_only_has_init() {
        // Partial bundle: only `distrobox-init` is present (no siblings).
        // The migration still succeeds: the entrypoint is linked, no sibling
        // `ln` calls are issued, and the post-condition passes.
        let runner = NullCommandRunnerBuilder::new()
            .cmd_full(ls_cmd(), || Ok("distrobox-init\n".to_string()))
            .build();
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
                format!("mkdir -p {}", STALE_DIR),
                format!("ln -sfn {} {}", CURRENT_INIT, STALE_INIT),
                format!("ls -1 {}", CURRENT_DIR),
                format!("test -e {}", STALE_INIT),
            ]
        );
    }

    #[test]
    fn test_migrate_stale_path_links_future_siblings_automatically() {
        // Forward-compat: if upstream adds a new `distrobox-*` script, it is
        // picked up by the directory listing without code changes here.
        let runner = NullCommandRunnerBuilder::new()
            .cmd_full(
                ls_cmd(),
                || Ok("distrobox-init\ndistrobox-export\ndistrobox-host-exec\ndistrobox-future-tool\n".to_string()),
            )
            .build();
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
                format!("mkdir -p {}", STALE_DIR),
                format!("ln -sfn {} {}", CURRENT_INIT, STALE_INIT),
                format!("ls -1 {}", CURRENT_DIR),
                format!(
                    "ln -sfn {}/distrobox-export {}/distrobox-export",
                    CURRENT_DIR, STALE_DIR
                ),
                format!(
                    "ln -sfn {}/distrobox-future-tool {}/distrobox-future-tool",
                    CURRENT_DIR, STALE_DIR
                ),
                format!(
                    "ln -sfn {}/distrobox-host-exec {}/distrobox-host-exec",
                    CURRENT_DIR, STALE_DIR
                ),
                format!("test -e {}", STALE_INIT),
            ]
        );
    }

    #[test]
    fn test_migrate_stale_path_links_no_siblings_when_ls_fails() {
        // `ls` failure (e.g. bundle dir missing) is non-fatal: the entrypoint
        // is still linked unconditionally, and the post-condition surfaces
        // the underlying provisioning failure if the current init is gone.
        let runner = NullCommandRunnerBuilder::new()
            .cmd_full_with_status(ls_cmd(), failing_status(), || Ok(String::new()))
            .build();
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
                format!("mkdir -p {}", STALE_DIR),
                format!("ln -sfn {} {}", CURRENT_INIT, STALE_INIT),
                format!("ls -1 {}", CURRENT_DIR),
                format!("test -e {}", STALE_INIT),
            ]
        );
    }

    #[test]
    fn test_migrate_stale_path_fails_when_sibling_ln_fails() {
        // Partial-state: the entrypoint links successfully, but a sibling
        // `ln -sfn` fails mid-loop. The migration must bail and name the
        // failing sibling path. (The entrypoint symlink is left in place;
        // re-running the migration is idempotent.)
        let export_current = format!("{}/distrobox-export", CURRENT_DIR);
        let export_stale = format!("{}/distrobox-export", STALE_DIR);
        let runner = NullCommandRunnerBuilder::new()
            .cmd_full(ls_cmd(), move || Ok(REALISTIC_BUNDLE_LS.to_string()))
            .cmd_full_with_status(
                Command::new_with_args(
                    "ln",
                    ["-sfn", export_current.as_str(), export_stale.as_str()],
                ),
                failing_status(),
                || Ok(String::new()),
            )
            .build();

        let result = block_on(migrate_stale_path(
            &runner,
            Path::new(STALE_INIT),
            Path::new(CURRENT_INIT),
        ));

        let err = result.expect_err("must bail when a sibling ln -sfn fails");
        let msg = err.to_string();
        assert!(
            msg.contains(&export_stale),
            "error must name the failing sibling path, got: {msg}"
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
        // The current entrypoint itself is missing from the bundle. The
        // entrypoint is linked unconditionally (creating a dangling symlink),
        // then the post-condition `test -e <stale_init>` fails and bails.
        // This is the provisioning-failure path: re-downloading the bundle
        // is the remedy, not a self-referencing symlink.
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

        let err = result.expect_err("must bail when the entrypoint doesn't resolve");
        let msg = err.to_string();
        assert!(
            msg.contains("does not resolve after migration"),
            "error must indicate the entrypoint did not resolve, got: {msg}"
        );
        assert!(
            msg.contains(CURRENT_INIT),
            "error must hint at the missing current init, got: {msg}"
        );
    }
}
