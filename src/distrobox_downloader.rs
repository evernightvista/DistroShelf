use crate::fakers::Command;
use crate::fakers::CommandRunner;
use crate::fakers::FileSystem;
use crate::models::DistroboxTask;
use crate::models::RootStore;
use anyhow::{Context, anyhow};
use gtk::glib;
use std::io;
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
pub fn resolve_bundled_distrobox_path(file_system: &FileSystem) -> Option<PathBuf> {
    ensure_stable_bundled_dir(file_system);
    let path = get_bundled_distrobox_path();
    if file_system.exists(&path) {
        Some(path)
    } else {
        None
    }
}

/// Returns true if the given installed version is strictly older than the
/// version DistroShelf currently ships (`DISTROBOX_VERSION`).
pub fn is_bundled_update_available(installed_version: &str) -> bool {
    version_less_than(installed_version, DISTROBOX_VERSION)
}

/// Ensures the stable bundled directory exists. If it doesn't but a legacy
/// versioned directory does, the legacy directory is *copied* (not moved) into
/// the stable path so that already-created containers — which may reference the
/// legacy absolute path — keep working.
pub(crate) fn ensure_stable_bundled_dir(file_system: &FileSystem) {
    let bundled_path = get_bundled_distrobox_path();
    if file_system.exists(&bundled_path) {
        return;
    }
    let Some((version, src_dir)) = find_latest_legacy_version_dir(file_system) else {
        return;
    };
    let stable_dir = get_stable_bundled_dir();
    if file_system
        .write(&get_version_file_path(), &version)
        .is_err()
    {
        tracing::warn!("Failed to write bundled version marker");
        return;
    }
    if copy_dir_via_fs(file_system, &src_dir, &stable_dir).is_err() {
        tracing::warn!("Failed to migrate bundled distrobox to stable path");
        return;
    }
    tracing::info!(
        "Migrated bundled distrobox {} to stable path {:?}",
        version,
        stable_dir
    );
}

pub(crate) fn copy_dir_via_fs(file_system: &FileSystem, src: &Path, dst: &Path) -> io::Result<()> {
    file_system.create_dir_all(dst)?;
    for entry in file_system.read_dir(src)? {
        let src_path = src.join(&entry);
        let dst_path = dst.join(&entry);
        match file_system.read_to_string(&src_path) {
            Ok(content) => {
                file_system.write(&dst_path, &content)?;
            }
            Err(_) => {
                copy_dir_via_fs(file_system, &src_path, &dst_path)?;
            }
        }
    }
    Ok(())
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
            Some((
                parts,
                version_str.to_string(),
                parent.join(name),
            ))
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
        .write(&stable_dir.join(VERSION_FILE), DISTROBOX_VERSION)
        .context("Failed to write version marker")?;

    // 5. Make executable (it should be already, but just in case)
    let binary_path = get_bundled_distrobox_path();
    log(
        &task,
        &format!(
            "Setting executable permissions on {:?}...",
            binary_path
        ),
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
    use std::path::Path;

    // ── copy_dir_via_fs ────────────────────────────────────────────

    #[test]
    fn test_copy_dir_via_fs_flat() {
        let fs = FileSystem::new_null();
        fs.create_dir_all(Path::new("/src")).unwrap();
        fs.write(Path::new("/src/a.txt"), "alpha").unwrap();
        fs.write(Path::new("/src/b.txt"), "beta").unwrap();

        copy_dir_via_fs(&fs, Path::new("/src"), Path::new("/dst")).unwrap();

        assert!(fs.exists(Path::new("/dst")));
        assert_eq!(
            fs.read_to_string(Path::new("/dst/a.txt")).unwrap(),
            "alpha"
        );
        assert_eq!(
            fs.read_to_string(Path::new("/dst/b.txt")).unwrap(),
            "beta"
        );
    }

    #[test]
    fn test_copy_dir_via_fs_nested() {
        let fs = FileSystem::new_null();
        fs.create_dir_all(Path::new("/src/sub/deep")).unwrap();
        fs.write(Path::new("/src/top.txt"), "top").unwrap();
        fs.write(Path::new("/src/sub/mid.txt"), "mid").unwrap();
        fs.write(Path::new("/src/sub/deep/bottom.txt"), "bottom").unwrap();

        copy_dir_via_fs(&fs, Path::new("/src"), Path::new("/dst")).unwrap();

        assert_eq!(
            fs.read_to_string(Path::new("/dst/top.txt")).unwrap(),
            "top"
        );
        assert_eq!(
            fs.read_to_string(Path::new("/dst/sub/mid.txt")).unwrap(),
            "mid"
        );
        assert_eq!(
            fs.read_to_string(Path::new("/dst/sub/deep/bottom.txt")).unwrap(),
            "bottom"
        );
    }

    #[test]
    fn test_copy_dir_via_fs_empty() {
        let fs = FileSystem::new_null();
        fs.create_dir_all(Path::new("/src")).unwrap();

        copy_dir_via_fs(&fs, Path::new("/src"), Path::new("/dst")).unwrap();

        assert!(fs.exists(Path::new("/dst")));
        let entries = fs.read_dir(Path::new("/dst")).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_copy_dir_via_fs_nonexistent_source() {
        let fs = FileSystem::new_null();
        let err =
            copy_dir_via_fs(&fs, Path::new("/nonexistent"), Path::new("/dst")).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    // ── ensure_stable_bundled_dir ──────────────────────────────────

    #[test]
    fn test_ensure_stable_bundled_dir_already_migrated() {
        let bundled_binary = get_bundled_distrobox_path();
        let fs = NullFileSystemBuilder::new()
            .file(&bundled_binary, "#!/bin/sh\necho distrobox")
            .build();

        ensure_stable_bundled_dir(&fs);

        assert!(fs.exists(&bundled_binary));
        assert_eq!(
            fs.read_to_string(&bundled_binary).unwrap(),
            "#!/bin/sh\necho distrobox"
        );
    }

    #[test]
    fn test_ensure_stable_bundled_dir_legacy_migration() {
        let base_dir = get_bundled_distrobox_dir();
        let legacy_dir = base_dir.join("distrobox-1.0.0");
        let legacy_binary = legacy_dir.join("distrobox");
        let stable_dir = get_stable_bundled_dir();
        let stable_binary = get_bundled_distrobox_path();
        let version_file = get_version_file_path();

        let fs = NullFileSystemBuilder::new()
            .dir(&base_dir)
            .dir(&legacy_dir)
            .file(&legacy_binary, "#!/bin/sh\necho distrobox")
            .build();

        ensure_stable_bundled_dir(&fs);

        assert!(fs.exists(&stable_binary));
        assert_eq!(
            fs.read_to_string(&stable_binary).unwrap(),
            "#!/bin/sh\necho distrobox"
        );
        assert!(fs.exists(&version_file));
        assert_eq!(fs.read_to_string(&version_file).unwrap(), "1.0.0");
        assert!(fs.exists(&stable_dir));
    }

    #[test]
    fn test_ensure_stable_bundled_dir_no_bundled() {
        let fs = FileSystem::new_null();

        ensure_stable_bundled_dir(&fs);

        let bundled_binary = get_bundled_distrobox_path();
        assert!(!fs.exists(&bundled_binary));
    }
}
