// File system abstraction with a real std::fs-backed variant and a Null
// variant with an in-memory HashMap, to ease code testing.
// Follows the same "Nullable" pattern as `Settings` and `CommandRunner`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io;
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
        self.files.borrow().contains_key(path)
    }
}

/// Builds a [`FileSystem::Null`] with predefined file contents.
#[derive(Clone, Default)]
pub struct NullFileSystemBuilder {
    files: HashMap<PathBuf, String>,
}

impl NullFileSystemBuilder {
    pub fn new() -> Self {
        Self {
            files: HashMap::new(),
        }
    }

    #[allow(dead_code)]
    pub fn file(&mut self, path: impl Into<PathBuf>, contents: impl Into<String>) -> &mut Self {
        self.files.insert(path.into(), contents.into());
        self
    }

    pub fn build(&self) -> FileSystem {
        FileSystem::Null(NullFileSystem {
            files: Rc::new(RefCell::new(self.files.clone())),
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
}
