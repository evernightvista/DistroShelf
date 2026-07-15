use super::VersionedExecutable;

/// The source and details of the selected distrobox executable.
#[derive(Clone, Debug)]
pub enum DistroboxExecutable {
    Host(VersionedExecutable),
    Bundled(VersionedExecutable),
}

impl DistroboxExecutable {
    pub fn version(&self) -> &str {
        match self {
            DistroboxExecutable::Host(exe) | DistroboxExecutable::Bundled(exe) => &exe.version,
        }
    }

    pub fn path(&self) -> &str {
        match self {
            DistroboxExecutable::Host(exe) | DistroboxExecutable::Bundled(exe) => &exe.path,
        }
    }

    pub fn is_bundled(&self) -> bool {
        matches!(self, DistroboxExecutable::Bundled(_))
    }
}
