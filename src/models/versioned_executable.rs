/// Information about an executable: its version string and filesystem path.
/// Both fields are required — a `VersionedExecutable` only exists when both
/// are known. Use `Option<VersionedExecutable>` to represent optional availability.
#[derive(Clone, Debug)]
pub struct VersionedExecutable {
    pub version: String,
    pub path: String,
}
