use serde::{Deserialize, Deserializer};
use std::{
    collections::BTreeMap, ffi::OsString, os::unix::ffi::OsStringExt, path::PathBuf, str::FromStr,
};

use crate::backends::desktop_file::{DesktopEntry, extract_quoted_string, parse_desktop_file};
use crate::fakers::Command;

#[derive(thiserror::Error, Debug)]
pub enum ParseError {
    #[error("{0}")]
    Parse(String),
}

pub fn to_hex(s: &str) -> String {
    s.bytes().map(|b| format!("{:02x}", b)).collect()
}

#[derive(Deserialize, Debug)]
pub struct DesktopFiles {
    #[serde(deserialize_with = "DesktopFiles::deserialize_path")]
    home_dir: PathBuf,
    #[serde(deserialize_with = "DesktopFiles::deserialize_desktop_files")]
    system: BTreeMap<PathBuf, String>,
    #[serde(deserialize_with = "DesktopFiles::deserialize_desktop_files")]
    user: BTreeMap<PathBuf, String>,
}

impl DesktopFiles {
    fn decode_hex<E: serde::de::Error>(hex_str: &str) -> Result<Vec<u8>, E> {
        if !hex_str.len().is_multiple_of(2) {
            return Err(E::invalid_length(
                hex_str.len(),
                &"hex string to have an even length",
            ));
        }

        (0..hex_str.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex_str[i..=i + 1], 16))
            .collect::<Result<_, _>>()
            .map_err(|e| {
                E::custom(format_args!(
                    "hex string contains non hex characters: {e:?}"
                ))
            })
    }

    fn decode_utf8_from_hex<E: serde::de::Error>(hex_str: &str) -> Result<String, E> {
        String::from_utf8(Self::decode_hex(hex_str)?).map_err(|e| {
            E::custom(format_args!(
                "decoded hex string does not represent valid UTF-8: {e:?}"
            ))
        })
    }

    fn decode_path_from_hex<E: serde::de::Error>(hex_str: &str) -> Result<PathBuf, E> {
        Ok(PathBuf::from(OsString::from_vec(Self::decode_hex(
            hex_str,
        )?)))
    }

    fn deserialize_path<'de, D: Deserializer<'de>>(deserializer: D) -> Result<PathBuf, D::Error> {
        Self::decode_path_from_hex(&String::deserialize(deserializer)?)
    }

    fn deserialize_desktop_files<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<BTreeMap<PathBuf, String>, D::Error> {
        BTreeMap::<String, String>::deserialize(deserializer)?
            .into_iter()
            .map(|(path, content)| {
                Ok((
                    Self::decode_path_from_hex(&path)?,
                    Self::decode_utf8_from_hex(&content)?,
                ))
            })
            .collect()
    }

    fn into_map(self, host_home: Option<PathBuf>) -> BTreeMap<PathBuf, String> {
        let mut desktop_files = self.system;
        // Only include user desktop files if the container's home directory is different from the host's
        // This avoids showing duplicate entries when the container shares the host's home directory
        if host_home.as_ref() != Some(&self.home_dir) {
            desktop_files.extend(self.user)
        }
        desktop_files
    }
}

pub fn decode_desktop_files(
    toml_str: &str,
    host_home: Option<PathBuf>,
) -> Result<BTreeMap<PathBuf, String>, ParseError> {
    let desktop_files: DesktopFiles =
        toml::from_str(toml_str).map_err(|e| ParseError::Parse(format!("{e:?}")))?;
    Ok(desktop_files.into_map(host_home))
}

#[derive(Clone, Debug, PartialEq, Hash)]
pub enum Status {
    Up(String),
    Created(String),
    Exited(String),
    // I don't want the app to crash if the parsing fails because distrobox changed with an update.
    // We will just disable some features, but still show the status value.
    Other(String),
}

impl Default for Status {
    fn default() -> Self {
        Self::Other("".into())
    }
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Status::Up(s) => write!(f, "Up {}", s),
            Status::Created(s) => write!(f, "Created {}", s),
            Status::Exited(s) => write!(f, "Exited {}", s),
            Status::Other(s) => write!(f, "{}", s),
        }
    }
}

impl Status {
    pub fn from_str(s: &str) -> Self {
        if let Some(rest) = s.strip_prefix("Up") {
            Status::Up(rest.trim().to_string())
        } else if let Some(rest) = s.strip_prefix("Exited") {
            Status::Exited(rest.trim().to_string())
        } else if let Some(rest) = s.strip_prefix("Created") {
            Status::Created(rest.trim().to_string())
        } else {
            Status::Other(s.to_string())
        }
    }
}

#[derive(Debug, PartialEq, Hash, Clone)]
pub struct ContainerInfo {
    pub id: String,
    pub name: String,
    pub status: Status,
    pub image: String,
    pub created_at: Option<String>,
    pub last_used_at: Option<String>,
}

impl ContainerInfo {
    fn field_missing_error(text: &str, line: &str) -> ParseError {
        ParseError::Parse(format!("{text} missing in line: {}", line))
    }
}

impl FromStr for ContainerInfo {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = s.split('|').collect();
        if parts.len() != 4 {
            return Err(ParseError::Parse(format!(
                "Invalid field count (expected 4, got {}) in line: {}",
                parts.len(),
                s
            )));
        }

        let id = parts[0].trim();
        let name = parts[1].trim();
        let status = parts[2].trim();
        let image = parts[3].trim();

        if id.is_empty() {
            return Err(ContainerInfo::field_missing_error("id", s));
        }
        if name.is_empty() {
            return Err(ContainerInfo::field_missing_error("name", s));
        }
        if status.is_empty() {
            return Err(ContainerInfo::field_missing_error("status", s));
        }
        if image.is_empty() {
            return Err(ContainerInfo::field_missing_error("image", s));
        }

        Ok(ContainerInfo {
            id: id.to_string(),
            name: name.to_string(),
            status: Status::from_str(status),
            image: image.to_string(),
            created_at: None,
            last_used_at: None,
        })
    }
}

#[derive(thiserror::Error, Debug)]
#[error("invalid value: {hint}")]
pub struct InvalidValue {
    pub hint: String,
}

#[derive(Default, Debug, PartialEq, Clone)]
pub struct CreateArgName(pub String);

impl std::fmt::Display for CreateArgName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl CreateArgName {
    pub fn new(value: &str) -> Result<Self, InvalidValue> {
        let re = regex::Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9_.-]*$").unwrap();
        if re.is_match(value) {
            Ok(CreateArgName(value.to_string()))
        } else {
            Err(InvalidValue {
                hint: "Must respect the format [a-zA-Z0-9][a-zA-Z0-9_.-]*".into(),
            })
        }
    }
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Hash)]
pub struct CreateArgsImage(String);

impl CreateArgsImage {
    pub fn new(value: &str) -> Result<Self, InvalidValue> {
        if value.trim().is_empty() {
            Err(InvalidValue {
                hint: "Image cannot be empty".into(),
            })
        } else {
            Ok(CreateArgsImage(value.to_string()))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CreateArgsImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Default, Debug, PartialEq, Clone)]
pub struct CreateArgs {
    pub init: bool,
    pub nvidia: bool,
    pub root: bool,
    pub no_entry: bool,
    pub hostname: Option<String>,
    pub home_path: Option<String>,
    pub image: Option<CreateArgsImage>,
    pub name: CreateArgName,
    pub volumes: Vec<Volume>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum VolumeMode {
    ReadOnly,
}

impl std::fmt::Display for VolumeMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VolumeMode::ReadOnly => write!(f, "ro"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Volume {
    pub host_path: String,
    pub container_path: String,
    pub mode: Option<VolumeMode>,
}

impl FromStr for Volume {
    type Err = InvalidValue;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = s.split(':').collect();
        match parts.as_slice() {
            [host] => Ok(Volume {
                host_path: host.to_string(),
                container_path: host.to_string(),
                mode: None,
            }),
            [host, target] => Ok(Volume {
                host_path: host.to_string(),
                container_path: target.to_string(),
                mode: None,
            }),
            [host, target, "ro"] => Ok(Volume {
                host_path: host.to_string(),
                container_path: target.to_string(),
                mode: Some(VolumeMode::ReadOnly),
            }),
            _ => Err(InvalidValue {
                hint: format!("Invalid volume descriptor: {}", s),
            }),
        }
    }
}

impl std::fmt::Display for Volume {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.host_path, self.container_path)?;
        if let Some(mode) = &self.mode {
            write!(f, ":{}", mode)?;
        }
        Ok(())
    }
}

pub fn create_cmd(args: &CreateArgs, mut base: Command) -> Command {
    base.arg("create").arg("--yes");
    if let Some(ref image) = args.image {
        base.arg("--image").arg(image.as_str());
    }
    if !args.name.0.is_empty() {
        base.arg("--name").arg(&args.name.0);
    }
    if let Some(ref hostname) = args.hostname {
        base.arg("--hostname").arg(hostname.as_str());
    }
    if args.init {
        base.arg("--init")
            .arg("--additional-packages")
            .arg("systemd");
    }
    if args.root {
        base.arg("--root");
    }
    if args.no_entry {
        base.arg("--no-entry");
    }
    if args.nvidia {
        base.arg("--nvidia");
    }
    if let Some(ref home_path) = args.home_path {
        base.arg("--home").arg(home_path.as_str());
    }
    for volume in &args.volumes {
        base.arg("--volume").arg(volume.to_string());
    }
    base
}

pub fn enter_cmd(name: &str, mut base: Command) -> Command {
    base.arg("enter").arg(name).arg("--no-workdir");
    base
}

pub fn assemble_cmd(file_path: &str, mut base: Command) -> Command {
    base.arg("assemble")
        .arg("create")
        .arg("--file")
        .arg(file_path);
    base
}

pub fn assemble_from_url_cmd(url: &str, mut base: Command) -> Command {
    base.arg("assemble").arg("create").arg("--file").arg(url);
    base
}

#[derive(Debug, Clone)]
pub struct ExportableApp {
    pub entry: DesktopEntry,
    pub desktop_file_path: String,
    pub exported: bool,
}

#[derive(Debug, Clone)]
pub struct ExportableBinary {
    pub name: String,
    pub source_path: String,
    pub exported_path: String,
}

pub fn assemble_exportable_apps(
    files: Vec<(String, String)>,
    box_name: &str,
    exported: Vec<String>,
) -> Vec<ExportableApp> {
    files
        .into_iter()
        .flat_map(|(path, content)| -> Option<ExportableApp> {
            let entry = match parse_desktop_file(&content) {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!("Failed to parse desktop file {}: {}", path, e);
                    return None;
                }
            };
            let file_name = std::path::Path::new(&path)
                .file_name()
                .and_then(|x| x.to_str())
                .unwrap_or_default();

            let exported_as = format!("{box_name}-{file_name}");
            let is_exported = exported.contains(&exported_as);
            if is_exported {
                tracing::debug!(found_exported = exported_as);
            }
            Some(ExportableApp {
                desktop_file_path: path,
                entry,
                exported: is_exported,
            })
        })
        .collect()
}

pub fn parse_exported_binaries_line(line: &str) -> Option<(String, String)> {
    if line.is_empty() || !line.contains('|') {
        return None;
    }
    let parts: Vec<&str> = line.split('|').collect();
    if parts.len() < 2 {
        return None;
    }
    let source_path = parts[0].trim().trim_matches('\'').to_string();
    let exported_path = parts[1].trim().to_string();
    if exported_path.is_empty() {
        return None;
    }
    Some((source_path, exported_path))
}

pub fn extract_binary_path_from_wrapper_content(content: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("exec")
            && let Some(path) = extract_quoted_string(trimmed, '\'')
            && path.starts_with('/')
            && !path.contains("distrobox")
        {
            return Some(path);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_parsing() {
        assert_eq!(
            Status::from_str("Up 2 hours"),
            Status::Up("2 hours".to_string())
        );
        assert_eq!(
            Status::from_str("Up (Paused)"),
            Status::Up("(Paused)".to_string())
        );

        assert_eq!(
            Status::from_str("Created 5 minutes ago"),
            Status::Created("5 minutes ago".to_string())
        );

        assert_eq!(
            Status::from_str("Exited (0) 10 seconds ago"),
            Status::Exited("(0) 10 seconds ago".to_string())
        );

        assert_eq!(
            Status::from_str("Unknown status"),
            Status::Other("Unknown status".to_string())
        );

        assert_eq!(Status::from_str(""), Status::Other("".to_string()));
    }

    #[test]
    fn status_display() {
        assert_eq!(Status::Up("2 hours".to_string()).to_string(), "Up 2 hours");
        assert_eq!(
            Status::Created("5 minutes ago".to_string()).to_string(),
            "Created 5 minutes ago"
        );
        assert_eq!(
            Status::Exited("(0) 10 seconds ago".to_string()).to_string(),
            "Exited (0) 10 seconds ago"
        );
        assert_eq!(Status::Other("Unknown".to_string()).to_string(), "Unknown");
    }

    #[test]
    fn container_info_parsing() -> Result<(), ParseError> {
        let line = "abc123 | my-container | Up 5 hours | docker.io/library/ubuntu:latest";
        let info = ContainerInfo::from_str(line)?;
        assert_eq!(info.id, "abc123");
        assert_eq!(info.name, "my-container");
        assert_eq!(info.status, Status::Up("5 hours".to_string()));
        assert_eq!(info.image, "docker.io/library/ubuntu:latest");

        let line =
            "def456 | fedora | Created 2 minutes ago | ghcr.io/ublue-os/fedora-toolbox:latest";
        let info = ContainerInfo::from_str(line)?;
        assert_eq!(info.id, "def456");
        assert_eq!(info.name, "fedora");
        assert_eq!(info.status, Status::Created("2 minutes ago".to_string()));
        assert_eq!(info.image, "ghcr.io/ublue-os/fedora-toolbox:latest");

        let line = "789ghi | arch | Exited (0) 1 day ago | docker.io/library/archlinux:latest";
        let info = ContainerInfo::from_str(line)?;
        assert_eq!(info.id, "789ghi");
        assert_eq!(info.name, "arch");
        assert_eq!(info.status, Status::Exited("(0) 1 day ago".to_string()));
        assert_eq!(info.image, "docker.io/library/archlinux:latest");

        Ok(())
    }

    #[test]
    fn container_info_parsing_errors() {
        let result = ContainerInfo::from_str("abc123 | my-container | Up");
        assert!(result.is_err());

        let result = ContainerInfo::from_str("a | b | c | d | e");
        assert!(result.is_err());

        let result = ContainerInfo::from_str(" | my-container | Up | image");
        assert!(result.is_err());

        let result = ContainerInfo::from_str("abc123 |  | Up | image");
        assert!(result.is_err());
    }

    fn make_desktop_files_toml(
        home_dir: &str,
        system_files: &[(&str, &str)],
        user_files: &[(&str, &str)],
    ) -> String {
        let mut toml = format!("home_dir=\"{}\"\n", to_hex(home_dir));

        toml.push_str("[system]\n");
        for (path, content) in system_files {
            toml.push_str(&format!("\"{}\"=\"{}\"\n", to_hex(path), to_hex(content)));
        }

        toml.push_str("[user]\n");
        for (path, content) in user_files {
            toml.push_str(&format!("\"{}\"=\"{}\"\n", to_hex(path), to_hex(content)));
        }

        toml
    }

    #[test]
    fn decode_desktop_files_basic() -> Result<(), ParseError> {
        let vim_desktop = "[Desktop Entry]\nType=Application\nName=Vim";
        let toml_str = make_desktop_files_toml(
            "/home/user",
            &[("/usr/share/applications/vim.desktop", vim_desktop)],
            &[],
        );
        let result = decode_desktop_files(&toml_str, None)?;
        assert_eq!(result.len(), 1);
        let (path, content) = result.first_key_value().unwrap();
        assert_eq!(
            path.to_string_lossy(),
            "/usr/share/applications/vim.desktop"
        );
        assert_eq!(content.as_str(), vim_desktop);
        Ok(())
    }

    #[test]
    fn decode_desktop_files_host_home_dedup() -> Result<(), ParseError> {
        let system_content = "[Desktop Entry]\nType=Application\nName=SystemApp";
        let user_content = "[Desktop Entry]\nType=Application\nName=UserApp";

        let toml_str = make_desktop_files_toml(
            "/home/user",
            &[("/usr/share/applications/system.desktop", system_content)],
            &[(
                "/home/user/.local/share/applications/user.desktop",
                user_content,
            )],
        );

        let result = decode_desktop_files(&toml_str, Some(PathBuf::from("/home/user")))?;
        assert_eq!(result.len(), 1);

        let result = decode_desktop_files(&toml_str, Some(PathBuf::from("/other/home")))?;
        assert_eq!(result.len(), 2);
        Ok(())
    }

    #[test]
    fn decode_desktop_files_invalid_toml() {
        let result = decode_desktop_files("not valid toml", None);
        assert!(result.is_err());
    }

    #[test]
    fn volume_parsing() -> Result<(), InvalidValue> {
        let vol = Volume::from_str("/data")?;
        assert_eq!(vol.host_path, "/data");
        assert_eq!(vol.container_path, "/data");
        assert_eq!(vol.mode, None);

        let vol = Volume::from_str("/host/path:/container/path")?;
        assert_eq!(vol.host_path, "/host/path");
        assert_eq!(vol.container_path, "/container/path");
        assert_eq!(vol.mode, None);

        let vol = Volume::from_str("/data:/data:ro")?;
        assert_eq!(vol.host_path, "/data");
        assert_eq!(vol.container_path, "/data");
        assert_eq!(vol.mode, Some(VolumeMode::ReadOnly));

        let result = Volume::from_str("/a:/b:/c:/d");
        assert!(result.is_err());

        Ok(())
    }

    #[test]
    fn volume_display() {
        let vol = Volume {
            host_path: "/host".to_string(),
            container_path: "/container".to_string(),
            mode: None,
        };
        assert_eq!(vol.to_string(), "/host:/container");

        let vol_ro = Volume {
            host_path: "/host".to_string(),
            container_path: "/container".to_string(),
            mode: Some(VolumeMode::ReadOnly),
        };
        assert_eq!(vol_ro.to_string(), "/host:/container:ro");
    }

    #[test]
    fn create_cmd_all_flags() {
        let args = CreateArgs {
            image: Some(CreateArgsImage::new("docker.io/library/ubuntu:latest").unwrap()),
            init: true,
            nvidia: true,
            root: true,
            hostname: Some("my-host".into()),
            home_path: Some("/home/me".into()),
            volumes: vec![
                Volume::from_str("/mnt/sdb1:/mnt/sdb1").unwrap(),
                Volume::from_str("/mnt/sdb4:/mnt/sdb4:ro").unwrap(),
            ],
            ..Default::default()
        };
        let cmd = create_cmd(&args, Command::new("distrobox"));
        let expected = "distrobox create --yes --image docker.io/library/ubuntu:latest --hostname my-host --init --additional-packages systemd --root --nvidia --home /home/me --volume /mnt/sdb1:/mnt/sdb1 --volume /mnt/sdb4:/mnt/sdb4:ro";
        assert_eq!(cmd.to_string(), expected);
    }

    #[test]
    fn create_cmd_with_no_entry() {
        let args = CreateArgs {
            image: Some(CreateArgsImage::new("docker.io/library/ubuntu:latest").unwrap()),
            no_entry: true,
            ..Default::default()
        };
        let cmd = create_cmd(&args, Command::new("distrobox"));
        assert!(cmd.to_string().contains(" --no-entry"));
    }

    #[test]
    fn enter_cmd_basic() {
        let cmd = enter_cmd("my-container", Command::new("distrobox"));
        assert_eq!(cmd.to_string(), "distrobox enter my-container --no-workdir");
    }

    #[test]
    fn assemble_cmd_basic() {
        let cmd = assemble_cmd("/path/to/assemble.yml", Command::new("distrobox"));
        assert_eq!(
            cmd.to_string(),
            "distrobox assemble create --file /path/to/assemble.yml"
        );
    }

    #[test]
    fn assemble_from_url_cmd_basic() {
        let cmd = assemble_from_url_cmd(
            "https://example.com/assemble.yml",
            Command::new("distrobox"),
        );
        assert_eq!(
            cmd.to_string(),
            "distrobox assemble create --file https://example.com/assemble.yml"
        );
    }

    #[test]
    fn create_arg_name_valid() {
        assert!(CreateArgName::new("my-container").is_ok());
        assert!(CreateArgName::new("my.container").is_ok());
        assert!(CreateArgName::new("my_container").is_ok());
        assert!(CreateArgName::new("my-container_1").is_ok());
    }

    #[test]
    fn create_arg_name_invalid() {
        assert!(CreateArgName::new("-bad").is_err());
        assert!(CreateArgName::new("").is_err());
        assert!(CreateArgName::new("bad name").is_err());
    }

    #[test]
    fn create_args_image_non_empty() {
        assert!(CreateArgsImage::new("ubuntu:latest").is_ok());
        assert!(CreateArgsImage::new("").is_err());
        assert!(CreateArgsImage::new("  ").is_err());
    }

    #[test]
    fn assemble_exportable_apps_matches_exported() {
        let vim_desktop =
            "[Desktop Entry]\nType=Application\nName=Vim\nExec=/usr/bin/vim\nIcon=vim";
        let fish_desktop =
            "[Desktop Entry]\nType=Application\nName=Fish\nExec=/usr/bin/fish\nIcon=fish";
        let files = vec![
            (
                "/usr/share/applications/vim.desktop".to_string(),
                vim_desktop.to_string(),
            ),
            (
                "/usr/share/applications/fish.desktop".to_string(),
                fish_desktop.to_string(),
            ),
        ];
        let exported = vec!["ubuntu-vim.desktop".to_string()];

        let apps = assemble_exportable_apps(files, "ubuntu", exported);
        assert_eq!(apps.len(), 2);
        assert!(apps[0].entry.name == "Vim" || apps[1].entry.name == "Vim");
        assert!(apps[0].entry.name == "Fish" || apps[1].entry.name == "Fish");

        let vim_app = apps.iter().find(|a| a.entry.name == "Vim").unwrap();
        assert!(vim_app.exported);
        let fish_app = apps.iter().find(|a| a.entry.name == "Fish").unwrap();
        assert!(!fish_app.exported);
    }

    #[test]
    fn assemble_exportable_apps_with_space_in_filename() {
        let proton_desktop = "[Desktop Entry]\nType=Application\nName=Proton Authenticator\nExec=/usr/bin/proton-authenticator %u\nIcon=proton-authenticator";
        let files = vec![(
            "/usr/share/applications/Proton Authenticator.desktop".to_string(),
            proton_desktop.to_string(),
        )];
        let exported = vec!["ubuntu-Proton Authenticator.desktop".to_string()];

        let apps = assemble_exportable_apps(files, "ubuntu", exported);
        assert_eq!(apps.len(), 1);
        assert_eq!(&apps[0].entry.name, "Proton Authenticator");
        assert_eq!(
            &apps[0].desktop_file_path,
            "/usr/share/applications/Proton Authenticator.desktop"
        );
        assert!(apps[0].exported);
    }

    #[test]
    fn assemble_exportable_apps_drops_unparseable() {
        let files = vec![(
            "/usr/share/applications/bad.desktop".to_string(),
            "not a desktop file".to_string(),
        )];
        let apps = assemble_exportable_apps(files, "ubuntu", vec![]);
        assert!(apps.is_empty());
    }

    #[test]
    fn parse_exported_binaries_line_normal() {
        let (source, exported) =
            parse_exported_binaries_line("'/usr/bin/vim'       | /home/user/.local/bin/vim")
                .unwrap();
        assert_eq!(source, "/usr/bin/vim");
        assert_eq!(exported, "/home/user/.local/bin/vim");
    }

    #[test]
    fn parse_exported_binaries_line_empty_source() {
        let (source, exported) =
            parse_exported_binaries_line("                    | /home/user/.local/bin/nvim")
                .unwrap();
        assert_eq!(source, "");
        assert_eq!(exported, "/home/user/.local/bin/nvim");
    }

    #[test]
    fn parse_exported_binaries_line_empty() {
        assert!(parse_exported_binaries_line("").is_none());
    }

    #[test]
    fn parse_exported_binaries_line_no_pipe() {
        assert!(parse_exported_binaries_line("some random text").is_none());
    }

    #[test]
    fn parse_exported_binaries_line_empty_exported_path() {
        assert!(parse_exported_binaries_line("'/usr/bin/vim'       | ").is_none());
    }

    #[test]
    fn extract_binary_path_from_wrapper_else_branch() {
        let content = r#"#!/bin/sh
# distrobox_binary
# name: archlinux
if [ -z "${CONTAINER_ID}" ]; then
	exec "distrobox-enter" -n archlinux -- '/usr/bin/nvim' "$@"
elif [ -n "${CONTAINER_ID}" ] && [ "${CONTAINER_ID}" != "archlinux" ]; then
	exec distrobox-host-exec '/home/user/.local/bin/nvim' "$@"
else
	exec '/usr/bin/nvim' "$@"
fi"#;
        let result = extract_binary_path_from_wrapper_content(content);
        // The third exec line (else branch) has '/usr/bin/nvim' which doesn't contain "distrobox"
        // The first exec line has '/usr/bin/nvim' inside a string with "distrobox-enter" — but
        // the path doesn't contain "distrobox", so it would match first.
        // The second line has distrobox-host-exec as command, not a path.
        assert_eq!(result.as_deref(), Some("/usr/bin/nvim"));
    }

    #[test]
    fn extract_binary_path_from_wrapper_no_match() {
        let content = "#!/bin/sh\necho hello\n";
        assert!(extract_binary_path_from_wrapper_content(content).is_none());
    }

    #[test]
    fn extract_binary_path_from_wrapper_rejects_distrobox_path() {
        let content = "exec '/usr/bin/distrobox-enter'\n";
        assert!(extract_binary_path_from_wrapper_content(content).is_none());
    }
}
