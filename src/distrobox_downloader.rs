use crate::fakers::Command;
use crate::fakers::CommandRunner;
use crate::fakers::FileSystem;
use crate::models::DistroboxTask;
use crate::models::RootStore;
use anyhow::{Context, anyhow};
use gtk::glib;
use std::path::PathBuf;

pub(crate) mod domain {
    use std::path::PathBuf;

    pub const DISTROBOX_VERSION: &str = "1.8.2.5";

    const BUNDLED_DIR_NAME: &str = "distrobox-bundled";
    const VERSION_FILE: &str = "VERSION";

    pub fn parse_semver(v: &str) -> Option<Vec<u32>> {
        v.split('.').map(|p| p.parse::<u32>().ok()).collect()
    }

    pub fn version_less_than(a: &str, b: &str) -> bool {
        match (parse_semver(a), parse_semver(b)) {
            (Some(ap), Some(bp)) => ap < bp,
            _ => false,
        }
    }

    /// Returns true if the given installed version is strictly older than the
    /// version DistroShelf currently ships (`DISTROBOX_VERSION`).
    pub fn is_bundled_update_available(installed_version: &str) -> bool {
        version_less_than(installed_version, DISTROBOX_VERSION)
    }

    pub fn get_bundled_distrobox_path() -> PathBuf {
        get_stable_bundled_dir().join("distrobox")
    }

    pub fn get_bundled_distrobox_dir() -> PathBuf {
        gtk::glib::user_data_dir().join("distroshelf")
    }

    /// The stable, version-independent directory that holds the bundled distrobox.
    pub fn get_stable_bundled_dir() -> PathBuf {
        get_bundled_distrobox_dir().join(BUNDLED_DIR_NAME)
    }

    /// Path to the file recording the installed bundled distrobox version.
    pub fn get_version_file_path() -> PathBuf {
        get_stable_bundled_dir().join(VERSION_FILE)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_parse_semver_basic() {
            assert_eq!(parse_semver("1.8.2"), Some(vec![1, 8, 2]));
            assert_eq!(parse_semver("1.0"), Some(vec![1, 0]));
            assert_eq!(parse_semver("not-a-version"), None);
            assert_eq!(parse_semver("1.x.3"), None);
        }

        #[test]
        fn test_version_less_than() {
            assert!(version_less_than("1.0.0", "2.0.0"));
            assert!(version_less_than("1.8.2", "1.8.3"));
            assert!(!version_less_than("2.0.0", "1.0.0"));
            assert!(!version_less_than("1.0.0", "1.0.0"));
            assert!(!version_less_than("not-semver", "1.0.0"));
            assert!(!version_less_than("1.0.0", "not-semver"));
        }

        #[test]
        fn test_is_bundled_update_available() {
            assert!(is_bundled_update_available("1.0.0"));
            assert!(!is_bundled_update_available(DISTROBOX_VERSION));
            assert!(!is_bundled_update_available("999.0.0"));
        }

        #[test]
        fn test_path_helpers() {
            let bundled = get_bundled_distrobox_path();
            assert!(bundled.ends_with("distrobox-bundled/distrobox"));

            let version_file = get_version_file_path();
            assert!(version_file.ends_with("distrobox-bundled/VERSION"));
        }
    }
}

use domain::*;
pub use domain::{
    DISTROBOX_VERSION, get_bundled_distrobox_dir, get_bundled_distrobox_path,
    is_bundled_update_available,
};

/// SHA256 of the tar.gz file from github
pub const DISTROBOX_SHA256: &str =
    "0c3bc4785ee3be3b89f93abb7cc0a9f60e56989e81319af140a4b60403b18f80";

/// Resolves the bundled distrobox binary path.
///
/// Resolution runs purely locally (no network): the stable
/// `distrobox-bundled/distrobox` path always wins when present; otherwise the
/// newest legacy versioned install (`distrobox-<VERSION>/distrobox`) is used
/// as a fallback. Legacy directories are never deleted or migrated by the
/// app, so a legacy-only install keeps working offline. This function only
/// *uses* existing directories — it never creates the stable dir.
///
/// Returns `None` only when no bundled distrobox (stable or legacy) is present.
///
/// The division of labor with the container-level migration: folder-level
/// resolution decides *where the app's bundle lives*; `distrobox_init_migration.rs`
/// repairs containers whose baked-in path vanished externally (deleted dir,
/// host distrobox uninstalled after a source switch).
pub fn resolve_bundled_distrobox_path(file_system: &FileSystem) -> Option<PathBuf> {
    let stable = get_bundled_distrobox_path();
    if file_system.exists(&stable) {
        return Some(stable);
    }
    find_latest_legacy_version_dir(file_system).map(|(_version, dir)| dir.join("distrobox"))
}

/// Scans for legacy `distrobox-<VERSION>/` directories and returns the version
/// string and path of the most recent one. The stable directory
/// (`distrobox-bundled`) is ignored automatically because `bundled` is not a
/// numeric version.
fn find_latest_legacy_version_dir(file_system: &FileSystem) -> Option<(String, PathBuf)> {
    let parent = get_bundled_distrobox_dir();

    let entries = file_system.read_dir(&parent).ok()?;

    let mut candidates: Vec<(Vec<u32>, String, PathBuf)> = entries
        .iter()
        .filter_map(|name| {
            let name_str = name.to_str()?;
            let version_str = name_str.strip_prefix("distrobox-")?;
            let parts = parse_semver(version_str)?;
            Some((parts, version_str.to_string(), parent.join(name)))
        })
        .collect();

    let mut valid = Vec::new();
    for (parts, version, path) in candidates.drain(..) {
        let distrobox_path = path.join("distrobox");
        if file_system.exists(&distrobox_path) {
            valid.push((parts, version, path));
        }
    }

    valid.sort_by(|a, b| a.0.cmp(&b.0));
    valid
        .last()
        .map(|(_, version, path)| (version.clone(), path.clone()))
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
    let file_system = root_store_weak
        .upgrade()
        .map(|store| store.file_system())
        .unwrap_or_else(FileSystem::new_real);
    let download_dir = get_bundled_distrobox_dir();
    let tarball_path = download_dir.join("distrobox.tar.gz");
    let url = format!(
        "https://github.com/89luca89/distrobox/archive/refs/tags/{}.tar.gz",
        DISTROBOX_VERSION
    );

    file_system
        .create_dir_all(&download_dir)
        .context("Failed to create download directory")?;

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
    file_system
        .remove_file(&tarball_path)
        .context("Failed to remove tarball")?;

    // 4. Move the extracted `distrobox-<VERSION>/` folder to the stable path so
    //    the absolute path baked into containers never changes across updates.
    let extracted_dir = download_dir.join(format!("distrobox-{}", DISTROBOX_VERSION));
    let stable_dir = get_stable_bundled_dir();
    log(
        &task,
        &format!("Installing to stable path {:?}...", stable_dir),
    );
    if file_system.exists(&stable_dir) {
        file_system
            .remove_dir_all(&stable_dir)
            .context("Failed to remove previous bundled distrobox")?;
    }
    file_system
        .rename(&extracted_dir, &stable_dir)
        .context("Failed to move bundled distrobox to stable path")?;
    file_system
        .write(&get_version_file_path(), DISTROBOX_VERSION)
        .context("Failed to write version marker")?;

    // 5. Make executable (it should be already, but just in case)
    let binary_path = get_bundled_distrobox_path();
    log(
        &task,
        &format!("Setting executable permissions on {:?}...", binary_path),
    );
    file_system
        .set_unix_executable(&binary_path)
        .context("Failed to set executable permissions")?;

    log(&task, "Distrobox installed successfully.");

    if let Some(root_store) = root_store_weak.upgrade() {
        root_store.bundled_distrobox_version().refetch();
        root_store.update_bundled_update_available();
        root_store.set_current_dialog(crate::models::DialogType::None);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fakers::NullFileSystemBuilder;

    // ── resolve_bundled_distrobox_path ─────────────────────────────

    #[test]
    fn test_resolve_prefers_stable_over_legacy() {
        let base_dir = get_bundled_distrobox_dir();
        let stable = get_bundled_distrobox_path();
        let legacy = base_dir.join("distrobox-9.9.9").join("distrobox");

        let fs = NullFileSystemBuilder::new()
            .file(&stable, "stable script")
            .file(&legacy, "legacy script")
            .build();

        assert_eq!(
            resolve_bundled_distrobox_path(&fs),
            Some(stable),
            "stable must win even when a semver-newer legacy dir exists"
        );
    }

    #[test]
    fn test_resolve_falls_back_to_newest_legacy() {
        let base_dir = get_bundled_distrobox_dir();
        let old = base_dir.join("distrobox-1.0.0").join("distrobox");
        let newer = base_dir.join("distrobox-1.8.2.4").join("distrobox");
        let newest = base_dir.join("distrobox-2.0.0").join("distrobox");

        let fs = NullFileSystemBuilder::new()
            .file(&old, "old script")
            .file(&newer, "newer script")
            .file(&newest, "newest script")
            .build();

        assert_eq!(
            resolve_bundled_distrobox_path(&fs),
            Some(newest),
            "the highest semver legacy dir must be picked"
        );
    }

    #[test]
    fn test_resolve_skips_legacy_dir_without_distrobox_file() {
        let base_dir = get_bundled_distrobox_dir();
        let legacy = base_dir.join("distrobox-1.0.0");
        let binary = legacy.join("distrobox");

        let fs = NullFileSystemBuilder::new().dir(&base_dir).dir(&legacy).build();
        assert!(!fs.exists(&binary));

        assert_eq!(resolve_bundled_distrobox_path(&fs), None);
    }

    #[test]
    fn test_resolve_none_when_nothing_installed() {
        let fs = FileSystem::new_null();

        assert_eq!(resolve_bundled_distrobox_path(&fs), None);
    }
}
