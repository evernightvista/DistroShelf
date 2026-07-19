// File system abstraction with a real std::fs-backed variant and a Null
// variant with an in-memory HashMap, to ease code testing.
// Follows the same "Nullable" pattern as `Settings` and `CommandRunner`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::HashSet;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// A single file-system instance shared across the whole app.
///
/// `Real` forwards to `std::fs`.
/// `Null` keeps file contents in memory, so tests and UI previews never
/// touch the host file system.
#[derive(Clone)]
pub enum FileSystem {
    Real,
    Null(NullFileSystem),
}

impl FileSystem {
    pub fn new_real() -> Self {
        FileSystem::Real
    }

    pub fn new_null() -> Self {
        NullFileSystemBuilder::new().build()
    }

    pub fn read_to_string(&self, path: &Path) -> io::Result<String> {
        match self {
            FileSystem::Real => std::fs::read_to_string(path),
            FileSystem::Null(null) => null.read_to_string(path),
        }
    }

    pub fn write(&self, path: &Path, contents: &str) -> io::Result<()> {
        match self {
            FileSystem::Real => std::fs::write(path, contents),
            FileSystem::Null(null) => null.write(path, contents),
        }
    }

    pub fn exists(&self, path: &Path) -> bool {
        match self {
            FileSystem::Real => path.exists(),
            FileSystem::Null(null) => null.exists(path),
        }
    }

    pub fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        match self {
            FileSystem::Real => std::fs::create_dir_all(path),
            FileSystem::Null(null) => {
                let mut dirs = null.dirs.borrow_mut();
                for ancestor in path.ancestors() {
                    dirs.insert(ancestor.to_path_buf());
                }
                Ok(())
            }
        }
    }

    pub fn remove_file(&self, path: &Path) -> io::Result<()> {
        match self {
            FileSystem::Real => std::fs::remove_file(path),
            FileSystem::Null(null) => null.remove_file(path),
        }
    }

    pub fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        match self {
            FileSystem::Real => std::fs::remove_dir_all(path),
            FileSystem::Null(null) => null.remove_dir_all(path),
        }
    }

    pub fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        match self {
            FileSystem::Real => std::fs::rename(from, to),
            FileSystem::Null(null) => null.rename(from, to),
        }
    }

    pub fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        match self {
            FileSystem::Real => {
                let mut entries = Vec::new();
                for entry in std::fs::read_dir(path)? {
                    let entry = entry?;
                    entries.push(entry.file_name().into());
                }
                Ok(entries)
            }
            FileSystem::Null(null) => null.read_dir(path),
        }
    }

    pub fn set_unix_executable(&self, path: &Path) -> io::Result<()> {
        match self {
            FileSystem::Real => {
                let metadata = std::fs::metadata(path)?;
                let mut perms = metadata.permissions();
                perms.set_mode(perms.mode() | 0o111);
                std::fs::set_permissions(path, perms)
            }
            FileSystem::Null(_) => Ok(()),
        }
    }
}

impl Default for FileSystem {
    fn default() -> Self {
        FileSystem::new_null()
    }
}

impl std::fmt::Debug for FileSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileSystem::Real => f.write_str("FileSystem::Real"),
            FileSystem::Null(null) => f
                .debug_tuple("FileSystem::Null")
                .field(&*null.files.borrow())
                .field(&*null.dirs.borrow())
                .finish(),
        }
    }
}

/// In-memory file-system storage. Clones share state. Missing files
/// return an `io::Error` of kind `NotFound`, mirroring real file-system
/// semantics.
#[derive(Clone, Default)]
pub struct NullFileSystem {
    files: Rc<RefCell<HashMap<PathBuf, String>>>,
    dirs: Rc<RefCell<HashSet<PathBuf>>>,
}

impl NullFileSystem {
    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        self.files
            .borrow()
            .get(path)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("{path:?}")))
    }

    fn write(&self, path: &Path, contents: &str) -> io::Result<()> {
        self.files
            .borrow_mut()
            .insert(path.to_path_buf(), contents.to_string());
        Ok(())
    }

    fn exists(&self, path: &Path) -> bool {
        self.files.borrow().contains_key(path) || self.dirs.borrow().contains(path)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        match self.files.borrow_mut().remove(path) {
            Some(_) => Ok(()),
            None => Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{path:?}"),
            )),
        }
    }

    fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        let mut files = self.files.borrow_mut();
        files.retain(|k, _| !k.starts_with(path));
        let mut dirs = self.dirs.borrow_mut();
        dirs.retain(|d| !d.starts_with(path));
        Ok(())
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let mut files = self.files.borrow_mut();
        let to_move: Vec<(PathBuf, String)> = files
            .iter()
            .filter(|(k, _)| k.starts_with(from))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (old_key, _value) in &to_move {
            files.remove(old_key);
        }
        for (old_key, value) in to_move {
            let suffix = old_key.strip_prefix(from).unwrap();
            let new_key = to.join(suffix);
            files.insert(new_key, value);
        }
        Ok(())
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        let files = self.files.borrow();
        let dirs = self.dirs.borrow();

        let has_matching_files = files.keys().any(|k| k.starts_with(path));
        let is_dir = dirs.contains(path);

        if !is_dir && !has_matching_files {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{path:?}"),
            ));
        }

        let mut seen: HashSet<PathBuf> = HashSet::new();
        for key in files.keys() {
            let Ok(suffix) = key.strip_prefix(path) else {
                continue;
            };
            if let Some(first) = suffix.iter().next() {
                seen.insert(first.into());
            }
        }
        for dir in dirs.iter() {
            if dir == path {
                continue;
            }
            let Ok(suffix) = dir.strip_prefix(path) else {
                continue;
            };
            if let Some(first) = suffix.iter().next() {
                seen.insert(first.into());
            }
        }
        let mut entries: Vec<PathBuf> = seen.into_iter().collect();
        entries.sort();
        Ok(entries)
    }
}

/// Builds a [`FileSystem::Null`] with predefined file contents.
#[derive(Clone, Default)]
pub struct NullFileSystemBuilder {
    files: HashMap<PathBuf, String>,
    dirs: HashSet<PathBuf>,
}

impl NullFileSystemBuilder {
    pub fn new() -> Self {
        Self {
            files: HashMap::new(),
            dirs: HashSet::new(),
        }
    }

    #[allow(dead_code)]
    pub fn file(&mut self, path: impl Into<PathBuf>, contents: impl Into<String>) -> &mut Self {
        self.files.insert(path.into(), contents.into());
        self
    }

    #[allow(dead_code)]
    pub fn dir(&mut self, path: impl Into<PathBuf>) -> &mut Self {
        self.dirs.insert(path.into());
        self
    }

    pub fn build(&self) -> FileSystem {
        FileSystem::Null(NullFileSystem {
            files: Rc::new(RefCell::new(self.files.clone())),
            dirs: Rc::new(RefCell::new(self.dirs.clone())),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_write_and_read_roundtrip() {
        let fs = FileSystem::new_null();
        let path = Path::new("terminals.json");
        fs.write(path, r#"{"name":"xterm"}"#).unwrap();
        assert_eq!(fs.read_to_string(path).unwrap(), r#"{"name":"xterm"}"#);
    }

    #[test]
    fn test_missing_file_returns_not_found() {
        let fs = FileSystem::new_null();
        let err = fs
            .read_to_string(Path::new("nonexistent.json"))
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn test_exists_behavior() {
        let fs = FileSystem::new_null();
        let path = Path::new("config.json");
        assert!(!fs.exists(path));
        fs.write(path, "data").unwrap();
        assert!(fs.exists(path));
    }

    #[test]
    fn test_clones_share_state() {
        let fs1 = FileSystem::new_null();
        let fs2 = fs1.clone();
        fs1.write(Path::new("shared.txt"), "hello").unwrap();
        assert_eq!(
            fs2.read_to_string(Path::new("shared.txt")).unwrap(),
            "hello"
        );
    }

    #[test]
    fn test_builder_predefines_files() {
        let fs = NullFileSystemBuilder::new()
            .file("/etc/config", "value1")
            .file("/var/data", "value2")
            .build();

        assert_eq!(
            fs.read_to_string(Path::new("/etc/config")).unwrap(),
            "value1"
        );
        assert_eq!(fs.read_to_string(Path::new("/var/data")).unwrap(), "value2");
    }

    #[test]
    fn test_real_variant_smoke() {
        let dir = std::env::temp_dir().join(format!("distroshelf-fs-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("test.txt");
        let fs = FileSystem::new_real();
        fs.write(&file, "real content").unwrap();
        assert!(fs.exists(&file));
        assert_eq!(fs.read_to_string(&file).unwrap(), "real content");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_default_is_null() {
        let fs = FileSystem::default();
        let path = Path::new("test.json");
        assert!(!fs.exists(path));
        fs.write(path, "x").unwrap();
        assert!(fs.exists(path));
    }

    #[test]
    fn test_create_dir_all_real() {
        let dir = std::env::temp_dir().join(format!("distroshelf-cda-{}", std::process::id()));
        let fs = FileSystem::new_real();
        assert!(!fs.exists(&dir));
        fs.create_dir_all(&dir).unwrap();
        assert!(dir.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_create_dir_all_null() {
        let fs = FileSystem::new_null();
        fs.create_dir_all(Path::new("/some/dir")).unwrap();
        assert!(fs.exists(Path::new("/some/dir")));
        assert!(fs.exists(Path::new("/some")));
        assert!(fs.exists(Path::new("/")));
        assert!(!fs.exists(Path::new("/nonexistent")));
    }

    #[test]
    fn test_nullfs_directory_tracking() {
        let fs = FileSystem::new_null();
        let dir = Path::new("/my/test/dir");
        fs.create_dir_all(dir).unwrap();
        assert!(fs.exists(dir));
        assert!(fs.exists(Path::new("/my/test")));
        assert!(fs.exists(Path::new("/my")));
        let entries = fs.read_dir(dir).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_remove_file_null() {
        let fs = FileSystem::new_null();
        let path = Path::new("test.txt");
        fs.write(path, "content").unwrap();
        assert!(fs.exists(path));
        fs.remove_file(path).unwrap();
        assert!(!fs.exists(path));
    }

    #[test]
    fn test_remove_file_null_not_found() {
        let fs = FileSystem::new_null();
        let err = fs.remove_file(Path::new("nonexistent.txt")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn test_remove_dir_all_null() {
        let fs = FileSystem::new_null();
        fs.write(Path::new("/tmp/a/file1.txt"), "a").unwrap();
        fs.write(Path::new("/tmp/a/file2.txt"), "b").unwrap();
        fs.write(Path::new("/tmp/b/file3.txt"), "c").unwrap();
        fs.remove_dir_all(Path::new("/tmp/a")).unwrap();
        assert!(!fs.exists(Path::new("/tmp/a/file1.txt")));
        assert!(!fs.exists(Path::new("/tmp/a/file2.txt")));
        assert!(fs.exists(Path::new("/tmp/b/file3.txt")));
    }

    #[test]
    fn test_rename_null() {
        let fs = FileSystem::new_null();
        fs.write(Path::new("/old/a/file.txt"), "hello").unwrap();
        fs.write(Path::new("/old/b/other.txt"), "world").unwrap();
        fs.write(Path::new("/other/keep.txt"), "keep").unwrap();
        fs.rename(Path::new("/old"), Path::new("/new")).unwrap();
        assert!(!fs.exists(Path::new("/old/a/file.txt")));
        assert!(fs.exists(Path::new("/new/a/file.txt")));
        assert_eq!(
            fs.read_to_string(Path::new("/new/a/file.txt")).unwrap(),
            "hello"
        );
        assert_eq!(
            fs.read_to_string(Path::new("/new/b/other.txt")).unwrap(),
            "world"
        );
        assert!(fs.exists(Path::new("/other/keep.txt")));
    }

    #[test]
    fn test_read_dir_null() {
        let fs = FileSystem::new_null();
        fs.write(Path::new("/data/a.txt"), "a").unwrap();
        fs.write(Path::new("/data/b.txt"), "b").unwrap();
        fs.write(Path::new("/data/sub/c.txt"), "c").unwrap();
        fs.write(Path::new("/other/d.txt"), "d").unwrap();
        let entries = fs.read_dir(Path::new("/data")).unwrap();
        assert_eq!(entries.len(), 3);
        assert!(entries.contains(&PathBuf::from("a.txt")));
        assert!(entries.contains(&PathBuf::from("b.txt")));
        assert!(entries.contains(&PathBuf::from("sub")));
    }

    #[test]
    fn test_set_unix_executable_null() {
        let fs = FileSystem::new_null();
        fs.set_unix_executable(Path::new("/bin/tool")).unwrap();
    }

    #[test]
    fn test_set_unix_executable_real() {
        let dir =
            std::env::temp_dir().join(format!("distroshelf-sue-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("test.sh");
        std::fs::write(&script, "#!/bin/sh\necho hi").unwrap();
        let fs = FileSystem::new_real();
        fs.set_unix_executable(&script).unwrap();
        let meta = std::fs::metadata(&script).unwrap();
        assert!(meta.permissions().mode() & 0o111 != 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
