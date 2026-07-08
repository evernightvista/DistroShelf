use gtk::glib;

/// Sort key for container list models.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, glib::Enum)]
#[enum_type(name = "ContainerSortKey")]
pub enum ContainerSortKey {
    #[default]
    Name,
    CreationDate,
    LastUsedDate,
}

impl ContainerSortKey {
    pub fn to_str(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::CreationDate => "creation-date",
            Self::LastUsedDate => "last-used",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "name" => Some(Self::Name),
            "creation-date" => Some(Self::CreationDate),
            "last-used" => Some(Self::LastUsedDate),
            _ => None,
        }
    }
}
