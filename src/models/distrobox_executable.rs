use super::VersionedExecutable;
use gtk::{gio, prelude::*};

/// The selected source of the distrobox executable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DistroboxSource {
    Host,
    Bundled,
}

impl DistroboxSource {
    pub fn from_setting(settings: &gio::Settings) -> Self {
        match settings.string("distrobox-executable").as_str() {
            "bundled" => Self::Bundled,
            _ => Self::Host,
        }
    }

    pub fn to_setting_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Bundled => "bundled",
        }
    }
}

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
