use crate::fakers::Command;
use crate::fakers::CommandRunner;
use crate::models::DistroboxTask;
use crate::models::RootStore;
use anyhow::{Context, anyhow};
use gtk::glib;
use std::path::{Path, PathBuf};

pub const DISTROBOX_VERSION: &str = "1.8.2.5";
// SHA256 of the tar.gz file from github
pub const DISTROBOX_SHA256: &str =
    "0c3bc4785ee3be3b89f93abb7cc0a9f60e56989e81319af140a4b60403b18f80";

/// Stable, version-independent directory name for the bundled distrobox.
///
/// Upstream `distrobox` resolves the absolute path to its sibling `distrobox-init`
/// script and bakes that path into the containers it creates. If that path
/// changed on every DistroShelf update (as it did when each version lived in its
/// own `distrobox-<VERSION>/` directory), already-created containers would break
/// the moment the old directory is removed. Keeping the bundled distrobox at a
/// single stable path ensures containers keep finding `distrobox-init` across
/// updates.
const BUNDLED_DIR_NAME: &str = "distrobox-bundled";
const VERSION_FILE: &str = "VERSION";

/// Information about a distrobox binary: its version string and filesystem path.
/// Used to display both system and bundled distrobox details in the preferences dialog.
#[derive(Clone, Default)]
pub struct DistroboxBinaryInfo {
    pub version: Option<String>,
    pub path: Option<String>,
}

pub fn get_bundled_distrobox_path() -> PathBuf {
    get_stable_bundled_dir().join("distrobox")
}

pub fn get_bundled_distrobox_dir() -> PathBuf {
    glib::user_data_dir().join("distroshelf")
}

/// The stable, version-independent directory that holds the bundled distrobox.
fn get_stable_bundled_dir() -> PathBuf {
    get_bundled_distrobox_dir().join(BUNDLED_DIR_NAME)
}

/// Path to the file recording the installed bundled distrobox version.
fn get_version_file_path() -> PathBuf {
    get_stable_bundled_dir().join(VERSION_FILE)
}

/// Resolves the bundled distrobox binary path, migrating legacy versioned
/// installs to the stable path on first use.
///
/// Returns `None` only when no bundled distrobox (stable or legacy) is present.
pub fn resolve_bundled_distrobox_path() -> Option<PathBuf> {
    ensure_stable_bundled_dir();
    let path = get_bundled_distrobox_path();
    if path.exists() {
        Some(path)
    } else {
        None
    }
}

/// Returns true if a bundled distrobox is installed whose version is strictly
/// older than the version DistroShelf currently ships (`DISTROBOX_VERSION`).
pub fn is_bundled_update_available() -> bool {
    match get_installed_bundled_version() {
        Some(installed) => version_less_than(&installed, DISTROBOX_VERSION),
        None => false,
    }
}

/// Extracts the version string of the installed bundled distrobox.
///
/// Reads the `VERSION` marker inside the stable directory; if that is missing
/// (e.g. before migration), falls back to scanning legacy `distrobox-<VERSION>/`
/// directories. Returns `None` when nothing is installed.
pub fn get_installed_bundled_version() -> Option<String> {
    if let Some(version) = read_installed_version_file() {
        return Some(version);
    }
    find_latest_legacy_version_dir().map(|(version, _)| version)
}

/// Returns [`DistroboxBinaryInfo`] for the bundled distrobox, computed synchronously
/// from the filesystem. When no bundled version is installed, both fields are `None`.
pub fn get_bundled_info() -> DistroboxBinaryInfo {
    DistroboxBinaryInfo {
        version: get_installed_bundled_version(),
        path: resolve_bundled_distrobox_path().map(|p| p.to_string_lossy().into_owned()),
    }
}

fn read_installed_version_file() -> Option<String> {
    let raw = std::fs::read_to_string(get_version_file_path()).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Ensures the stable bundled directory exists. If it doesn't but a legacy
/// versioned directory does, the legacy directory is *copied* (not moved) into
/// the stable path so that already-created containers — which may reference the
/// legacy absolute path — keep working.
fn ensure_stable_bundled_dir() {
    if get_bundled_distrobox_path().exists() {
        return;
    }
    let Some((version, src_dir)) = find_latest_legacy_version_dir() else {
        return;
    };
    let stable_dir = get_stable_bundled_dir();
    // Record the version before copying so that a partial migration
    // (VERSION written but copy failed) is harmless — the stable binary
    // won't exist, so the next call retries the whole migration.
    if let Err(e) = std::fs::write(get_version_file_path(), &version) {
        tracing::warn!("Failed to write bundled version marker: {}", e);
        return;
    }
    if let Err(e) = copy_dir_recursive(&src_dir, &stable_dir) {
        tracing::warn!("Failed to migrate bundled distrobox to stable path: {}", e);
        return;
    }
    tracing::info!(
        "Migrated bundled distrobox {} to stable path {:?}",
        version,
        stable_dir
    );
}

/// Recursively copies a directory tree, replacing the destination if present.
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    if dst.exists() {
        std::fs::remove_dir_all(dst)?;
    }
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Scans for legacy `distrobox-<VERSION>/` directories and returns the version
/// string and path of the most recent one. The stable directory
/// (`distrobox-bundled`) is ignored automatically because `bundled` is not a
/// numeric version.
fn find_latest_legacy_version_dir() -> Option<(String, PathBuf)> {
    let parent = get_bundled_distrobox_dir();
    let entries = std::fs::read_dir(&parent).ok()?;

    let mut versions: Vec<(Vec<u32>, String, PathBuf)> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name();
            let name_str = name.to_str()?;
            let version_str = name_str.strip_prefix("distrobox-")?;
            let parts = parse_semver(version_str)?;
            if !entry.path().join("distrobox").exists() {
                return None;
            }
            Some((parts, version_str.to_string(), entry.path()))
        })
        .collect();

    versions.sort_by(|a, b| a.0.cmp(&b.0));
    versions.last().map(|(_, version, path)| (version.clone(), path.clone()))
}

fn parse_semver(v: &str) -> Option<Vec<u32>> {
    v.split('.').map(|p| p.parse::<u32>().ok()).collect()
}

fn version_less_than(a: &str, b: &str) -> bool {
    match (parse_semver(a), parse_semver(b)) {
        (Some(ap), Some(bp)) => ap < bp,
        _ => false,
    }
}

fn log(task: &DistroboxTask, msg: &str) {
    task.append_output(msg);
    task.append_output("\n");
}

pub async fn download_distrobox(
    task: DistroboxTask,
    root_store_weak: glib::WeakRef<RootStore>,
) -> anyhow::Result<()> {
    let command_runner = root_store_weak
        .upgrade()
        .map(|store| store.command_runner())
        .unwrap_or_else(CommandRunner::new_real);
    let download_dir = get_bundled_distrobox_dir();
    let tarball_path = download_dir.join("distrobox.tar.gz");
    let url = format!(
        "https://github.com/89luca89/distrobox/archive/refs/tags/{}.tar.gz",
        DISTROBOX_VERSION
    );

    // Ensure directory exists
    std::fs::create_dir_all(&download_dir).context("Failed to create download directory")?;

    log(
        &task,
        &format!("Using download directory: {:?}", download_dir),
    );

    // 1. Download
    log(&task, &format!("Downloading {}...", url));
    let mut curl_cmd = Command::new("curl");
    curl_cmd.arg("-L");
    curl_cmd.arg("-o");
    curl_cmd.arg(&tarball_path);
    curl_cmd.arg(&url);
    curl_cmd.stdout = crate::fakers::FdMode::Pipe;
    curl_cmd.stderr = crate::fakers::FdMode::Pipe;

    let child = command_runner
        .spawn(curl_cmd)
        .context("Failed to run curl")?;

    task.handle_child_output(child).await?;

    // 2. Verify SHA256
    log(&task, "Verifying checksum...");
    let mut sha_cmd = Command::new("sha256sum");
    sha_cmd.arg(&tarball_path);
    sha_cmd.stdout = crate::fakers::FdMode::Pipe;
    sha_cmd.stderr = crate::fakers::FdMode::Pipe;

    let output = command_runner.output(sha_cmd).await?;
    if !output.status.success() {
        return Err(anyhow!("sha256sum failed"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let calculated_hash = stdout.split_whitespace().next().unwrap_or_default();

    if calculated_hash != DISTROBOX_SHA256 {
        return Err(anyhow!(
            "Checksum mismatch. Expected {}, got {}",
            DISTROBOX_SHA256,
            calculated_hash
        ));
    }
    log(&task, "Checksum verified.");

    // 3. Extract
    log(&task, "Extracting...");
    let mut tar_cmd = Command::new("tar");
    tar_cmd.arg("xzf");
    tar_cmd.arg(&tarball_path);
    tar_cmd.arg("-C");
    tar_cmd.arg(&download_dir);
    tar_cmd.stdout = crate::fakers::FdMode::Pipe;
    tar_cmd.stderr = crate::fakers::FdMode::Pipe;

    let child = command_runner.spawn(tar_cmd).context("Failed to run tar")?;

    task.handle_child_output(child).await?;

    // 3b. Clean up tarball
    log(&task, "Removing tarball...");
    std::fs::remove_file(&tarball_path).context("Failed to remove tarball")?;

    // 4. Move the extracted `distrobox-<VERSION>/` folder to the stable path so
    //    the absolute path baked into containers never changes across updates.
    let extracted_dir = download_dir.join(format!("distrobox-{}", DISTROBOX_VERSION));
    let stable_dir = get_stable_bundled_dir();
    log(&task, &format!("Installing to stable path {:?}...", stable_dir));
    if stable_dir.exists() {
        std::fs::remove_dir_all(&stable_dir)
            .context("Failed to remove previous bundled distrobox")?;
    }
    std::fs::rename(&extracted_dir, &stable_dir)
        .context("Failed to move bundled distrobox to stable path")?;
    std::fs::write(stable_dir.join(VERSION_FILE), DISTROBOX_VERSION)
        .context("Failed to write version marker")?;

    // 5. Make executable (it should be already, but just in case)
    let binary_path = get_bundled_distrobox_path();
    log(
        &task,
        &format!("Setting executable permissions on {:?}...", binary_path),
    );

    let mut chmod_cmd = Command::new("chmod");
    chmod_cmd.arg("+x");
    chmod_cmd.arg(&binary_path);
    chmod_cmd.stdout = crate::fakers::FdMode::Pipe;
    chmod_cmd.stderr = crate::fakers::FdMode::Pipe;

    let output = command_runner.output(chmod_cmd).await?;
    if !output.status.success() {
        return Err(anyhow!("chmod failed"));
    }

    log(&task, "Distrobox installed successfully.");

    if let Some(root_store) = root_store_weak.upgrade() {
        root_store.distrobox_version().refetch();
        root_store.update_bundled_update_available();
        root_store.set_current_dialog(crate::models::DialogType::None);
    }

    Ok(())
}
