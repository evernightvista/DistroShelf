use crate::fakers::{Child, Command, CommandRunner, FdMode, NullCommandRunnerBuilder};

use std::{
    cell::LazyCell,
    collections::{BTreeMap, HashMap},
    io,
    path::{Path, PathBuf},
    process::Output,
    rc::Rc,
};
use tracing::{debug, error, info, warn};

use super::domain::{
    ContainerInfo, CreateArgs, ExportableApp, ExportableBinary, InvalidValue, Status,
    assemble_cmd as domain_assemble_cmd, assemble_exportable_apps,
    assemble_from_url_cmd as domain_assemble_from_url_cmd, create_cmd as domain_create_cmd,
    decode_desktop_files, enter_cmd as domain_enter_cmd, extract_binary_path_from_wrapper_content,
    parse_exported_binaries_line, to_hex,
};
use crate::backends::distrobox::command::{CmdFactory, default_cmd_factory};
use crate::backends::distrobox::fetch_distrobox_version;

const POSIX_FIND_AND_CONCAT_DESKTOP_FILES: &str =
    include_str!("POSIX_FIND_AND_CONCAT_DESKTOP_FILES.sh");

#[derive(Clone)]
pub struct Distrobox {
    cmd_runner: CommandRunner,
    cmd_factory: CmdFactory,
}

type CommandResponse = (Command, Rc<dyn Fn() -> io::Result<String>>);

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("failed to read command stdout: {0}")]
    StdoutRead(#[from] io::Error),

    #[error("failed to spawn command {command}: {source}")]
    Spawn { source: io::Error, command: String },

    #[error("failed to parse command output: {0}")]
    ParseOutput(String),

    #[error("{0}")]
    InvalidValue(#[from] InvalidValue),

    #[error("command failed with exit code {exit_code:?}: {command}\n{stderr}")]
    CommandFailed {
        exit_code: Option<i32>,
        command: String,
        stderr: String,
    },

    #[error("failed to resolve host path: {0}. getfattr may not be installed on the host")]
    ResolveHostPath(String),
}

/// Represents mock responses for the NullCommandRunner used in previews and testing.
///
/// These responses simulate the output of various distrobox commands without
/// actually executing them. This is essential for:
/// - UI previews in development (via DistroboxStoreTy::NullHostWorking)
/// - Unit testing without requiring a real distrobox installation
/// - Flatpak sandbox testing
#[derive(Clone)]
pub enum DistroboxCommandRunnerResponse {
    /// Mock response for `distrobox version` command
    /// Returns a successful version string like "distrobox: 1.7.2.1"
    Version,
    /// Mock response for when distrobox is not installed
    /// Returns an error when version is queried
    NoVersion,
    /// Mock response for `distrobox ls --no-color` command
    /// Returns a list of containers in the expected pipe-delimited format
    List(Vec<ContainerInfo>),
    /// Mock response for `distrobox create --compatibility` command
    /// Returns a list of compatible container images
    Compatibility(Vec<String>),
    /// Mock response for listing exportable applications from a container
    /// Contains: (distrobox_name, [(filename, app_name, icon_name)])
    /// Generates the TOML hex-encoded format expected by the desktop file parser
    ExportedApps(String, Vec<(String, String, String)>),
}

impl DistroboxCommandRunnerResponse {
    pub fn common_distros() -> LazyCell<Vec<ContainerInfo>> {
        LazyCell::new(|| {
            [
                ("1", "Ubuntu", "docker.io/library/ubuntu:latest"),
                ("2", "Fedora", "docker.io/library/fedora:latest"),
                ("3", "Kali", "docker.io/kalilinux/kali-rolling"),
                ("4", "Debian", "docker.io/library/debian:latest"),
                ("5", "Arch Linux", "docker.io/library/archlinux:latest"),
                ("6", "CentOS", "docker.io/library/centos:latest"),
                ("7", "Alpine", "docker.io/library/alpine:latest"),
                ("8", "OpenSUSE", "docker.io/library/opensuse:latest"),
                ("9", "Gentoo", "docker.io/library/gentoo:latest"),
                ("10", "Slackware", "docker.io/library/slackware:latest"),
                ("11", "Void Linux", "docker.io/library/voidlinux:latest"),
                ("13", "Deepin", "docker.io/library/deepin:latest"),
                ("16", "Rocky Linux", "docker.io/library/rockylinux:latest"),
                (
                    "17",
                    "Crystal Linux",
                    "docker.io/library/crystal-linux:latest",
                ),
            ]
            .iter()
            .map(|(id, name, image)| ContainerInfo {
                id: id.to_string(),
                name: name.to_string(),
                status: Status::Created("2 minutes ago".into()),
                image: image.to_string(),
                created_at: None,
                last_used_at: None,
            })
            .collect()
        })
    }

    pub fn new_list_common_distros() -> Self {
        Self::List(Self::common_distros().to_owned())
    }

    pub fn new_common_exported_apps() -> Self {
        let dummy_exported_apps = vec![
            ("vim.desktop".into(), "Vim".into(), "vim".into()),
            ("matlab.desktop".into(), "MATLAB".into(), "matlab".into()),
            (
                "vscode.desktop".into(),
                "Visual Studio Code".into(),
                "code".into(),
            ),
            ("rstudio.desktop".into(), "RStudio".into(), "rstudio".into()),
            (
                "sublime_text.desktop".into(),
                "Sublime Text".into(),
                "subl".into(),
            ),
            ("zoom.desktop".into(), "Zoom".into(), "zoom".into()),
            ("slack.desktop".into(), "Slack".into(), "slack".into()),
            ("postman.desktop".into(), "Postman".into(), "postman".into()),
        ];
        DistroboxCommandRunnerResponse::ExportedApps("Ubuntu".into(), dummy_exported_apps)
    }

    pub fn new_common_images() -> Self {
        DistroboxCommandRunnerResponse::Compatibility(
            Self::common_distros()
                .iter()
                .map(|x| x.image.clone())
                .collect(),
        )
    }

    fn build_version_response() -> (Command, String) {
        let mut cmd = default_cmd_factory()();
        cmd.arg("version");
        (cmd, "distrobox: 1.7.2.1".to_string())
    }

    fn build_no_version_response() -> (Command, Rc<dyn Fn() -> io::Result<String>>) {
        let mut cmd = default_cmd_factory()();
        cmd.arg("version");
        (cmd, Rc::new(|| Err(io::Error::from_raw_os_error(0))))
    }

    fn build_list_response(containers: &[ContainerInfo]) -> (Command, String) {
        let mut output = String::new();
        output.push_str("ID           | NAME                 | STATUS             | IMAGE  \n");
        for container in containers {
            output.push_str(&container.id);
            output.push_str(" | ");
            output.push_str(&container.name);
            output.push_str(" | ");
            let status = container.status.to_string();
            output.push_str(&format!("{status} | "));
            output.push_str(&container.image);
            output.push('\n');
        }
        let mut cmd = default_cmd_factory()();
        cmd.arg("ls").arg("--no-color");
        (cmd, output.clone())
    }

    fn build_compatibility_response(images: &[String]) -> (Command, String) {
        let output = images.join("\n");
        let mut cmd = default_cmd_factory()();
        cmd.arg("create").arg("--compatibility");
        (cmd, output)
    }

    fn build_exported_apps_commands(
        box_name: &str,
        apps: &[(String, String, String)],
    ) -> Vec<(Command, String)> {
        let mut commands = Vec::new();

        // Get XDG_DATA_HOME (mocked via printenv)
        commands.push((
            Command::new_with_args("printenv", ["XDG_DATA_HOME"]),
            String::new(),
        ));

        // Get HOME if XDG_DATA_HOME is empty (mocked via printenv)
        commands.push((
            Command::new_with_args("printenv", ["HOME"]),
            "/home/me".to_string(),
        ));

        // List desktop files - these are the exported files in the user's local applications folder
        // Format: {box_name}-{filename}
        let file_list = apps
            .iter()
            .map(|(filename, _, _)| format!("{box_name}-{}", filename))
            .collect::<Vec<_>>()
            .join("\n");
        commands.push((
            Command::new_with_args("ls", ["/home/me/.local/share/applications"]),
            file_list,
        ));

        // Build desktop files TOML with hex encoding (matching POSIX_FIND_AND_CONCAT_DESKTOP_FILES.sh output)
        let mut toml = format!("home_dir=\"{}\"\n", to_hex("/home/me"));

        toml.push_str("[system]\n");
        for (filename, name, icon) in apps {
            let path = format!("/usr/share/applications/{}", filename);
            let content = format!(
                "[Desktop Entry]\n\
                Type=Application\n\
                Name={}\n\
                Exec=/path/to/{}\n\
                Icon={}\n\
                Categories=Utility;Network;",
                name, name, icon
            );
            toml.push_str(&format!("\"{}\"=\"{}\"\n", to_hex(&path), to_hex(&content)));
        }

        toml.push_str("[user]\n");

        let mut db_cmd = default_cmd_factory()();
        db_cmd.args([
            "enter",
            box_name,
            "--",
            "sh",
            "-c",
            POSIX_FIND_AND_CONCAT_DESKTOP_FILES,
        ]);
        commands.push((db_cmd, toml));

        commands
    }

    fn wrap_err_fn(output: (Command, String)) -> CommandResponse {
        (output.0, Rc::new(move || Ok(output.1.clone())))
    }

    pub fn into_commands(self) -> Vec<CommandResponse> {
        match self {
            Self::Version => {
                let working_response = Self::build_version_response();
                vec![Self::wrap_err_fn(working_response)]
            }
            Self::NoVersion => {
                vec![Self::build_no_version_response()]
            }
            Self::List(containers) => {
                vec![Self::wrap_err_fn(Self::build_list_response(&containers))]
            }
            Self::Compatibility(images) => vec![Self::wrap_err_fn(
                Self::build_compatibility_response(&images),
            )],
            Self::ExportedApps(box_name, apps) => {
                Self::build_exported_apps_commands(&box_name, &apps)
                    .into_iter()
                    .map(Self::wrap_err_fn)
                    .collect()
            }
        }
    }
}

impl Distrobox {
    // The command factory ensures we can customize the distrobox executable path, e.g. to use a bundled version.
    pub fn new(cmd_runner: CommandRunner, cmd_factory: CmdFactory) -> Self {
        Self {
            cmd_runner,
            cmd_factory,
        }
    }

    fn dbcmd(&self) -> Command {
        (self.cmd_factory)()
    }

    pub fn command_runner(&self) -> &CommandRunner {
        &self.cmd_runner
    }

    pub fn null_command_runner(responses: &[DistroboxCommandRunnerResponse]) -> CommandRunner {
        let mut builder = NullCommandRunnerBuilder::new();
        for res in responses {
            for (cmd, out) in res.clone().into_commands() {
                builder.cmd_full(cmd, move || out());
            }
        }
        builder.build()
    }

    pub fn cmd_spawn(&self, mut cmd: Command) -> Result<Box<dyn Child + Send>, Error> {
        cmd.stdout = FdMode::Pipe;
        cmd.stderr = FdMode::Pipe;

        let program = cmd.program.to_string_lossy().to_string();
        let args = cmd
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>();

        debug!(command = %program, args = ?args, "Spawning command");
        let child = self.cmd_runner.spawn(cmd.clone()).map_err(|e| {
            let full_command = format!("{:?} {:?}", program, args);
            error!(error = ?e, command = %full_command, "Command spawn failed");
            Error::Spawn {
                source: e,
                command: full_command,
            }
        })?;

        Ok(child)
    }

    async fn cmd_output(&self, mut cmd: Command) -> Result<Output, Error> {
        cmd.stdout = FdMode::Pipe;
        cmd.stderr = FdMode::Pipe;

        let program = cmd.program.to_string_lossy().to_string();
        let args = cmd
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>();

        info!(command = %program, args = ?args, "Executing command");
        let command_str = format!("{:?} {:?}", program, args);

        let output = self.cmd_runner.output(cmd).await.map_err(|e| {
            error!(error = ?e, command = %program, "Command execution failed");
            Error::Spawn {
                source: e,
                command: command_str.clone(),
            }
        })?;

        let exit_code = output.status.code();
        debug!(
            exit_code = ?exit_code,
            "Command completed successfully"
        );
        Ok(output)
    }

    async fn cmd_output_string(&self, cmd: Command) -> Result<String, Error> {
        let command_str = format!("{:?} {:?}", cmd.program, cmd.args);
        let output = self.cmd_output(cmd).await?;
        let s = String::from_utf8_lossy(&output.stdout);

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            let exit_code = output.status.code();
            error!(
                exit_code = ?exit_code,
                stderr = %stderr,
                "Command failed"
            );
            return Err(Error::CommandFailed {
                exit_code,
                command: command_str,
                stderr,
            });
        }

        Ok(s.to_string())
    }

    async fn host_applications_path(
        &self,
        host_env: &HashMap<String, String>,
    ) -> Result<PathBuf, Error> {
        let xdg_data_home_opt = host_env
            .get("XDG_DATA_HOME")
            .filter(|s| !s.trim().is_empty())
            .map(|s| Path::new(s.trim()).to_path_buf());

        let apps_base = if let Some(p) = xdg_data_home_opt {
            p
        } else {
            // Fallback to HOME
            match host_env.get("HOME").filter(|s| !s.trim().is_empty()) {
                Some(s) => Path::new(s.trim()).join(".local/share"),
                None => {
                    return Err(Error::ResolveHostPath(
                        "XDG_DATA_HOME and HOME are not set on the host".into(),
                    ));
                }
            }
        };

        let apps_path = apps_base.join("applications");
        Ok(apps_path)
    }
    async fn get_exported_desktop_files(
        &self,
        host_env: &HashMap<String, String>,
    ) -> Result<Vec<String>, Error> {
        // We do everything with the command line to ensure we can access the files and environment variables
        // even when inside a flatpak sandbox, with only the permissions to run `flatpak-spawn`
        let mut cmd = Command::new("ls");
        cmd.arg(self.host_applications_path(host_env).await?);
        let ls_out = self.cmd_output_string(cmd).await?;
        let apps = ls_out
            .trim()
            .split("\n")
            .map(|app| app.to_string())
            .collect::<Vec<_>>();
        Ok(apps)
    }

    async fn get_desktop_files(
        &self,
        box_name: &str,
        host_env: &HashMap<String, String>,
    ) -> Result<Vec<(String, String)>, Error> {
        let mut cmd = self.dbcmd();
        cmd.args([
            "enter",
            box_name,
            "--",
            "sh",
            "-c",
            POSIX_FIND_AND_CONCAT_DESKTOP_FILES,
        ]);
        let toml_str = self.cmd_output_string(cmd).await?;
        let host_home_opt = host_env.get("HOME").cloned().map(PathBuf::from);
        let desktop_files = decode_desktop_files(&toml_str, host_home_opt)
            .map_err(|e| Error::ParseOutput(e.to_string()))?;
        debug!(desktop_files = ?desktop_files);

        Ok(desktop_files
            .into_iter()
            .map(|(path, content)| (path.to_string_lossy().into_owned(), content))
            .collect::<Vec<_>>())
    }

    pub async fn list_apps(&self, box_name: &str) -> Result<Vec<ExportableApp>, Error> {
        let host_env = match crate::fakers::resolve_host_env(&self.cmd_runner).await {
            Ok(env) => env,
            Err(e) => {
                tracing::warn!("failed to resolve host env via CommandRunner: {e:?}");
                HashMap::new()
            }
        };

        let files = self.get_desktop_files(box_name, &host_env).await?;
        debug!(desktop_files=?files);
        let exported = self.get_exported_desktop_files(&host_env).await?;
        debug!(exported_files=?exported);

        Ok(assemble_exportable_apps(files, box_name, exported))
    }

    /// Lists only the binaries that have already been exported from the container.
    pub async fn get_exported_binaries(
        &self,
        box_name: &str,
    ) -> Result<Vec<ExportableBinary>, Error> {
        let mut cmd = self.dbcmd();
        cmd.args([
            "enter",
            box_name,
            "--",
            "distrobox-export",
            "--list-binaries",
        ]);
        // Example output: '/usr/bin/vim' | /home/user/.local/bin/vim
        let output = self.cmd_output_string(cmd).await?;
        debug!(binaries_output = output);

        let mut binaries = Vec::new();
        for line in output.lines() {
            if let Some((source_path, exported_path)) = parse_exported_binaries_line(line) {
                let source_path = if source_path.is_empty() {
                    self.extract_binary_path_from_wrapper(&exported_path)
                        .await
                        .unwrap_or_else(|| exported_path.clone())
                } else {
                    source_path
                };

                let name = Path::new(&source_path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .filter(|s| !s.is_empty())
                    .or_else(|| {
                        Path::new(&exported_path)
                            .file_name()
                            .and_then(|n| n.to_str())
                    })
                    .unwrap_or(&source_path)
                    .to_string();

                binaries.push(ExportableBinary {
                    name,
                    source_path,
                    exported_path,
                });
            }
        }

        Ok(binaries)
    }

    /// Extracts the original binary path from a distrobox exported wrapper script.
    /// The wrapper script contains lines like: exec '/usr/bin/binary' "$@"
    async fn extract_binary_path_from_wrapper(&self, wrapper_path: &str) -> Option<String> {
        let cmd = Command::new_with_args("cat", [wrapper_path]);
        let output = self.cmd_output_string(cmd).await.ok()?;
        extract_binary_path_from_wrapper_content(&output)
    }

    pub fn launch_app(
        &self,
        container: &str,
        app: &ExportableApp,
    ) -> Result<Box<dyn Child + Send>, Error> {
        let mut cmd = self.dbcmd();
        cmd.arg("enter").arg("--name").arg(container).arg("--");
        let to_be_replaced = [" %f", " %u", " %F", " %U"];
        let cleaned_exec = to_be_replaced
            .into_iter()
            .fold(app.entry.exec.clone(), |acc, x| acc.replace(x, ""));
        cmd.arg(cleaned_exec);
        self.cmd_spawn(cmd)
    }

    pub async fn export_app(
        &self,
        container: &str,
        desktop_file_path: &str,
    ) -> Result<String, Error> {
        let mut cmd = self.dbcmd();
        cmd.args(["enter", "--name", container]).extend(
            "--",
            &Command::new_with_args("distrobox-export", ["--app", desktop_file_path]),
        );

        self.cmd_output_string(cmd).await
    }
    pub async fn unexport_app(
        &self,
        container: &str,
        desktop_file_path: &str,
    ) -> Result<String, Error> {
        let mut cmd = self.dbcmd();
        cmd.args(["enter", "--name", container]).extend(
            "--",
            &Command::new_with_args("distrobox-export", ["-d", "--app", desktop_file_path]),
        );

        self.cmd_output_string(cmd).await
    }

    pub async fn export_binary(
        &self,
        container: &str,
        binary_name_or_path: &str,
    ) -> Result<String, Error> {
        // Check if the input is a path or just a binary name
        // If it doesn't contain a '/' it's likely just a binary name
        let resolved_path = if !binary_name_or_path.contains('/') {
            // Resolve the binary name to its full path using 'which'
            self.resolve_binary_path(container, binary_name_or_path)
                .await?
        } else {
            binary_name_or_path.to_string()
        };

        let mut cmd = self.dbcmd();
        cmd.args(["enter", "--name", container]).extend(
            "--",
            &Command::new_with_args("distrobox-export", ["--bin", &resolved_path]),
        );

        self.cmd_output_string(cmd).await
    }

    /// Resolves a binary name to its full path using 'which' inside the container
    async fn resolve_binary_path(
        &self,
        container: &str,
        binary_name: &str,
    ) -> Result<String, Error> {
        let mut cmd = self.dbcmd();
        cmd.args(["enter", "--name", container, "--", "which", binary_name]);

        let output = self.cmd_output_string(cmd).await?;
        let path = output.trim();

        if path.is_empty() {
            return Err(Error::CommandFailed {
                exit_code: Some(1),
                command: format!("which {}", binary_name),
                stderr: format!("Binary '{}' not found in container", binary_name),
            });
        }

        Ok(path.to_string())
    }

    pub async fn unexport_binary(
        &self,
        container: &str,
        binary_path: &str,
    ) -> Result<String, Error> {
        let mut cmd = self.dbcmd();
        cmd.args(["enter", "--name", container]).extend(
            "--",
            &Command::new_with_args("distrobox-export", ["-d", "--bin", binary_path]),
        );

        self.cmd_output_string(cmd).await
    }

    // assemble
    pub fn assemble(&self, file_path: &str) -> Result<Box<dyn Child + Send>, Error> {
        if file_path.is_empty() {
            return Err(Error::InvalidValue(InvalidValue {
                hint: "File path cannot be empty".into(),
            }));
        }
        let cmd = domain_assemble_cmd(file_path, self.dbcmd());
        self.cmd_spawn(cmd)
    }

    pub fn assemble_from_url(&self, url: &str) -> Result<Box<dyn Child + Send>, Error> {
        if url.is_empty() {
            return Err(Error::InvalidValue(InvalidValue {
                hint: "URL cannot be empty".into(),
            }));
        }
        let cmd = domain_assemble_from_url_cmd(url, self.dbcmd());
        self.cmd_spawn(cmd)
    }
    fn create_cmd(&self, args: CreateArgs) -> Command {
        domain_create_cmd(&args, self.dbcmd())
    }
    // create
    pub async fn create(&self, args: CreateArgs) -> Result<Box<dyn Child + Send>, Error> {
        let cmd = self.create_cmd(args);
        self.cmd_spawn(cmd)
    }
    // create --compatibility
    pub async fn list_images(&self) -> Result<Vec<String>, Error> {
        let mut cmd = self.dbcmd();
        cmd.arg("create").arg("--compatibility");
        let text = self.cmd_output_string(cmd).await?;
        let lines = text
            .lines()
            .filter_map(|x| {
                if !x.is_empty() {
                    Some(x.to_string())
                } else {
                    None
                }
            })
            .collect();
        Ok(lines)
    }
    // enter
    pub fn enter_cmd(&self, name: &str) -> Command {
        domain_enter_cmd(name, self.dbcmd())
    }
    // clone from an existing container using create args to customize the clone
    pub async fn clone_from(
        &self,
        source_name: &str,
        args: CreateArgs,
    ) -> Result<Box<dyn Child + Send>, Error> {
        let mut cmd = self.create_cmd(args);
        cmd.remove_flag_value_arg("--image");
        cmd.arg("--clone").arg(source_name);
        self.cmd_spawn(cmd)
    }
    // list | ls
    pub async fn list(&self) -> Result<BTreeMap<String, ContainerInfo>, Error> {
        let mut cmd = self.dbcmd();
        cmd.arg("ls").arg("--no-color");
        let text = self.cmd_output_string(cmd).await?;
        let lines = text.lines().skip(1);
        let mut out = BTreeMap::new();
        for line in lines {
            match line.parse::<ContainerInfo>() {
                Ok(item) => {
                    debug!(
                        container_id = %item.id,
                        container_name = %item.name,
                        image = %item.image,
                        status = ?item.status,
                        "Discovered container"
                    );
                    out.insert(item.name.clone(), item);
                }
                Err(e) => {
                    error!(error = %e, line = %line, "Failed to parse container info");
                    return Err(Error::ParseOutput(e.to_string()));
                }
            }
        }
        Ok(out)
    }
    // rm
    pub async fn remove(&self, name: &str) -> Result<String, Error> {
        let mut cmd = self.dbcmd();
        cmd.arg("rm").arg("--force").arg(name);
        self.cmd_output_string(cmd).await
    }
    // stop
    pub async fn stop(&self, name: &str) -> Result<String, Error> {
        let mut cmd = self.dbcmd();
        cmd.arg("stop").arg("--yes").arg(name);
        self.cmd_output_string(cmd).await
    }
    pub async fn stop_all(&self) -> Result<String, Error> {
        let mut cmd = self.dbcmd();
        cmd.arg("stop").arg("--all").arg("--yes");
        self.cmd_output_string(cmd).await
    }
    // upgrade
    pub fn upgrade(&self, name: &str) -> Result<Box<dyn Child + Send>, Error> {
        let mut cmd = self.dbcmd();
        cmd.arg("upgrade").arg(name);

        self.cmd_spawn(cmd)
    }
    pub async fn upgrade_all(&mut self) -> Result<String, Error> {
        let mut cmd = self.dbcmd();
        cmd.arg("upgrade").arg("--all");
        self.cmd_output_string(cmd).await
    }
    // ephemeral
    // generate-entry
    // version
    pub async fn version(&self) -> Result<String, Error> {
        let version = fetch_distrobox_version(&self.cmd_runner, &self.cmd_factory)
            .await
            .inspect_err(|error| warn!(error = ?error, "Failed to detect distrobox version"))?;
        info!(
            distrobox_version = %version,
            "Successfully parsed distrobox version"
        );
        Ok(version)
    }

    // help
}

impl Default for Distrobox {
    fn default() -> Self {
        Self::new(CommandRunner::new_null(), default_cmd_factory())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::distrobox::{ContainerInfo, CreateArgsImage, Status, Volume};
    use smol::block_on;
    use std::str::FromStr;

    /// Helper to generate TOML output matching the shell script format
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
    fn list() -> Result<(), Error> {
        block_on(async {
            let output = "ID           | NAME                 | STATUS             | IMAGE                         
d24405b14180 | ubuntu               | Created            | ghcr.io/ublue-os/ubuntu-toolbox:latest";
            let db = Distrobox::new(
                NullCommandRunnerBuilder::new()
                    .cmd(&["distrobox", "ls", "--no-color"], output)
                    .build(),
                default_cmd_factory(),
            );
            assert_eq!(
                db.list().await?,
                BTreeMap::from_iter([(
                    "ubuntu".into(),
                    ContainerInfo {
                        id: "d24405b14180".into(),
                        name: "ubuntu".into(),
                        status: Status::Created("".into()),
                        image: "ghcr.io/ublue-os/ubuntu-toolbox:latest".into(),
                        created_at: None,
                        last_used_at: None,
                    }
                )])
            );
            Ok(())
        })
    }

    #[test]
    fn version() -> Result<(), Error> {
        block_on(async {
            let output = "distrobox: 1.7.2.1";
            let db = Distrobox::new(
                NullCommandRunnerBuilder::new()
                    .cmd(&["distrobox", "version"], output)
                    .build(),
                default_cmd_factory(),
            );
            assert_eq!(db.version().await?, "1.7.2.1".to_string(),);
            Ok(())
        })
    }

    #[test]
    fn version_falls_back_to_flag_for_v2() -> Result<(), Error> {
        use std::os::unix::process::ExitStatusExt;
        block_on(async {
            let db = Distrobox::new(
                NullCommandRunnerBuilder::new()
                    .cmd_full_with_status(
                        Command::new_with_args("distrobox", ["version"]),
                        ExitStatusExt::from_raw(3),
                        || Ok("No help topic for 'version'".to_string()),
                    )
                    .cmd(&["distrobox", "--version"], "distrobox version 2.0.0-rc.4")
                    .build(),
                default_cmd_factory(),
            );
            assert_eq!(db.version().await?, "2.0.0-rc.4".to_string());
            Ok(())
        })
    }

    #[test]
    fn version_errors_when_no_probe_yields_a_version() {
        block_on(async {
            let db = Distrobox::new(
                NullCommandRunnerBuilder::new().build(),
                default_cmd_factory(),
            );
            assert!(matches!(db.version().await, Err(Error::ParseOutput(_))));
        })
    }

    #[test]
    fn list_apps() -> Result<(), Error> {
        let vim_desktop = "[Desktop Entry]
Type=Application
Name=Vim
Exec=/path/to/vim
Icon=/path/to/icon.png
Comment=A brief description of my application
Categories=Utility;Network;";

        let fish_desktop = "[Desktop Entry]
Type=Application
Name=Fish
Exec=/path/to/fish
Icon=/path/to/icon.png
Comment=A brief description of my application
Categories=Utility;Network;";

        let desktop_files_toml = make_desktop_files_toml(
            "/home/me",
            &[
                ("/usr/share/applications/vim.desktop", vim_desktop),
                ("/usr/share/applications/fish.desktop", fish_desktop),
            ],
            &[],
        );

        let db = Distrobox::new(
            NullCommandRunnerBuilder::new()
                .cmd(
                    &["env", "-0"],
                    "HOME=/home/me\0XDG_DATA_HOME=/home/me/.local/share\0",
                )
                .cmd(
                    &["ls", "/home/me/.local/share/applications"],
                    "ubuntu-vim.desktop\n",
                )
                .cmd(
                    &[
                        "distrobox",
                        "enter",
                        "ubuntu",
                        "--",
                        "sh",
                        "-c",
                        POSIX_FIND_AND_CONCAT_DESKTOP_FILES,
                    ],
                    &desktop_files_toml,
                )
                .build(),
            default_cmd_factory(),
        );

        let apps = block_on(db.list_apps("ubuntu"))?;
        assert_eq!(&apps[0].entry.name, "Fish");
        assert_eq!(&apps[0].entry.exec, "/path/to/fish");
        assert!(!apps[0].exported);
        assert_eq!(&apps[1].entry.name, "Vim");
        assert_eq!(&apps[1].entry.exec, "/path/to/vim");
        assert!(apps[1].exported);
        Ok(())
    }

    #[test]
    fn list_apps_with_space_in_filename() -> Result<(), Error> {
        // Simulate a desktop file with a space in its filename and ensure it's parsed/export-detected correctly
        let proton_desktop = "[Desktop Entry]
Type=Application
Name=Proton Authenticator
Exec=/usr/bin/proton-authenticator %u
Icon=proton-authenticator
Categories=Utility;Security;";

        let desktop_files_toml = make_desktop_files_toml(
            "/home/me",
            &[(
                "/usr/share/applications/Proton Authenticator.desktop",
                proton_desktop,
            )],
            &[],
        );

        let db = Distrobox::new(
            NullCommandRunnerBuilder::new()
                .cmd(
                    &["env", "-0"],
                    "HOME=/home/me\0XDG_DATA_HOME=/home/me/.local/share\0",
                )
                .cmd(
                    &["ls", "/home/me/.local/share/applications"],
                    "ubuntu-Proton Authenticator.desktop\n",
                )
                .cmd(
                    &[
                        "distrobox",
                        "enter",
                        "ubuntu",
                        "--",
                        "sh",
                        "-c",
                        POSIX_FIND_AND_CONCAT_DESKTOP_FILES,
                    ],
                    &desktop_files_toml,
                )
                .build(),
            default_cmd_factory(),
        );

        let apps = block_on(db.list_apps("ubuntu"))?;
        assert_eq!(apps.len(), 1);
        assert_eq!(&apps[0].entry.name, "Proton Authenticator");
        assert_eq!(&apps[0].entry.exec, "/usr/bin/proton-authenticator %u");
        assert_eq!(
            &apps[0].desktop_file_path,
            "/usr/share/applications/Proton Authenticator.desktop"
        );
        // Ensure exported detection matches the filename with space
        assert!(apps[0].exported);
        Ok(())
    }

    #[test]
    fn create() -> Result<(), Error> {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        let db = Distrobox::new(CommandRunner::new_null(), default_cmd_factory());
        let output_tracker = db.cmd_runner.output_tracker();
        debug!("Testing container creation");
        let args = CreateArgs {
            image: Some(CreateArgsImage::new("docker.io/library/ubuntu:latest").unwrap()),
            init: true,
            nvidia: true,
            root: true,
            hostname: Some("my-host".into()),
            home_path: Some("/home/me".into()),
            volumes: vec![
                Volume::from_str("/mnt/sdb1:/mnt/sdb1")?,
                Volume::from_str("/mnt/sdb4:/mnt/sdb4:ro")?,
            ],
            ..Default::default()
        };
        smol::block_on(db.create(args))?;
        let expected = "distrobox create --yes --image docker.io/library/ubuntu:latest --hostname my-host --init --additional-packages systemd --root --nvidia --home /home/me --volume /mnt/sdb1:/mnt/sdb1 --volume /mnt/sdb4:/mnt/sdb4:ro";
        assert_eq!(
            output_tracker.items()[0].command().unwrap().to_string(),
            expected
        );
        Ok(())
    }

    #[test]
    fn create_with_no_entry() -> Result<(), Error> {
        let db = Distrobox::new(CommandRunner::new_null(), default_cmd_factory());
        let output_tracker = db.cmd_runner.output_tracker();
        let args = CreateArgs {
            image: Some(CreateArgsImage::new("docker.io/library/ubuntu:latest").unwrap()),
            no_entry: true,
            ..Default::default()
        };

        smol::block_on(db.create(args))?;

        let command = output_tracker.items()[0].command().unwrap().to_string();
        assert!(command.contains(" --no-entry"));
        Ok(())
    }

    #[test]
    fn assemble() -> Result<(), Error> {
        let db = Distrobox::new(CommandRunner::new_null(), default_cmd_factory());
        let output_tracker = db.cmd_runner.output_tracker();
        db.assemble("/path/to/assemble.yml")?;
        assert_eq!(
            output_tracker.items()[0].command().unwrap().to_string(),
            "distrobox assemble create --file /path/to/assemble.yml"
        );
        Ok(())
    }

    #[test]
    fn remove() -> Result<(), Error> {
        let db = Distrobox::new(CommandRunner::new_null(), default_cmd_factory());
        let output_tracker = db.cmd_runner.output_tracker();
        block_on(db.remove("ubuntu"))?;
        assert_eq!(
            output_tracker.items()[0].command().unwrap().to_string(),
            "distrobox rm --force ubuntu"
        );
        Ok(())
    }

    #[test]
    fn stub_responses() {
        let cmd_outputs = DistroboxCommandRunnerResponse::new_list_common_distros().into_commands();
        assert_eq!(
            cmd_outputs[0].1().unwrap(),
            "ID           | NAME                 | STATUS             | IMAGE  
1 | Ubuntu | Created 2 minutes ago | docker.io/library/ubuntu:latest
2 | Fedora | Created 2 minutes ago | docker.io/library/fedora:latest
3 | Kali | Created 2 minutes ago | docker.io/kalilinux/kali-rolling
4 | Debian | Created 2 minutes ago | docker.io/library/debian:latest
5 | Arch Linux | Created 2 minutes ago | docker.io/library/archlinux:latest
6 | CentOS | Created 2 minutes ago | docker.io/library/centos:latest
7 | Alpine | Created 2 minutes ago | docker.io/library/alpine:latest
8 | OpenSUSE | Created 2 minutes ago | docker.io/library/opensuse:latest
9 | Gentoo | Created 2 minutes ago | docker.io/library/gentoo:latest
10 | Slackware | Created 2 minutes ago | docker.io/library/slackware:latest
11 | Void Linux | Created 2 minutes ago | docker.io/library/voidlinux:latest
13 | Deepin | Created 2 minutes ago | docker.io/library/deepin:latest
16 | Rocky Linux | Created 2 minutes ago | docker.io/library/rockylinux:latest
17 | Crystal Linux | Created 2 minutes ago | docker.io/library/crystal-linux:latest\n"
        );
    }

    #[test]
    fn stub_exported_apps_generates_valid_toml() {
        let exported_apps = DistroboxCommandRunnerResponse::new_common_exported_apps();
        let commands = exported_apps.into_commands();

        let toml_command = commands
            .iter()
            .find(|(cmd, _)| {
                cmd.program.to_string_lossy().contains("distrobox")
                    && cmd
                        .args
                        .iter()
                        .any(|arg: &std::ffi::OsString| arg.to_string_lossy() == "enter")
            })
            .expect("Should have a TOML-generating command");

        let toml_output = toml_command.1().expect("Should generate output");

        let desktop_files = decode_desktop_files(&toml_output, None)
            .expect("Generated TOML should be valid and parseable");

        assert!(
            !desktop_files.is_empty(),
            "Should have system desktop files"
        );

        for (path, content) in &desktop_files {
            assert!(
                path.to_string_lossy().ends_with(".desktop"),
                "Path should end with .desktop: {:?}",
                path
            );
            assert!(
                content.contains("[Desktop Entry]"),
                "Content should be a valid desktop entry"
            );
            assert!(
                content.contains("Name="),
                "Content should have a Name field"
            );
        }
    }

    #[test]
    fn get_exported_binaries_parses_normal_output() -> Result<(), Error> {
        block_on(async {
            // Normal output with source path present
            let list_output = "'/usr/bin/vim'       | /home/user/.local/bin/vim\n'/usr/bin/htop'      | /home/user/.local/bin/htop";
            let db = Distrobox::new(
                NullCommandRunnerBuilder::new()
                    .cmd(
                        &[
                            "distrobox",
                            "enter",
                            "test-box",
                            "--",
                            "distrobox-export",
                            "--list-binaries",
                        ],
                        list_output,
                    )
                    .build(),
                default_cmd_factory(),
            );
            let binaries = db.get_exported_binaries("test-box").await?;
            assert_eq!(binaries.len(), 2);
            assert_eq!(binaries[0].name, "vim");
            assert_eq!(binaries[0].source_path, "/usr/bin/vim");
            assert_eq!(binaries[0].exported_path, "/home/user/.local/bin/vim");
            assert_eq!(binaries[1].name, "htop");
            assert_eq!(binaries[1].source_path, "/usr/bin/htop");
            Ok(())
        })
    }

    #[test]
    fn get_exported_binaries_handles_empty_source_path() -> Result<(), Error> {
        block_on(async {
            // Output with empty source path (distrobox bug when sudo_prefix is empty)
            // In this case, the wrapper script should be read to extract the actual path
            let list_output = "                    | /home/user/.local/bin/nvim";
            let wrapper_content = r#"#!/bin/sh
# distrobox_binary
# name: archlinux
if [ -z "${CONTAINER_ID}" ]; then
	exec "distrobox-enter" -n archlinux -- '/usr/bin/nvim' "$@"
elif [ -n "${CONTAINER_ID}" ] && [ "${CONTAINER_ID}" != "archlinux" ]; then
	exec distrobox-host-exec '/home/user/.local/bin/nvim' "$@"
else
	exec '/usr/bin/nvim' "$@"
fi"#;
            let db = Distrobox::new(
                NullCommandRunnerBuilder::new()
                    .cmd(
                        &[
                            "distrobox",
                            "enter",
                            "archlinux",
                            "--",
                            "distrobox-export",
                            "--list-binaries",
                        ],
                        list_output,
                    )
                    .cmd(&["cat", "/home/user/.local/bin/nvim"], wrapper_content)
                    .build(),
                default_cmd_factory(),
            );
            let binaries = db.get_exported_binaries("archlinux").await?;
            assert_eq!(binaries.len(), 1);
            assert_eq!(binaries[0].name, "nvim");
            assert_eq!(binaries[0].source_path, "/usr/bin/nvim");
            assert_eq!(binaries[0].exported_path, "/home/user/.local/bin/nvim");
            Ok(())
        })
    }

    #[test]
    fn get_exported_binaries_fallback_to_exported_path_name() -> Result<(), Error> {
        block_on(async {
            // Output with empty source path and wrapper script that can't be read
            let list_output = "                    | /home/user/.local/bin/my-tool";
            let db = Distrobox::new(
                NullCommandRunnerBuilder::new()
                    .cmd(
                        &[
                            "distrobox",
                            "enter",
                            "test-box",
                            "--",
                            "distrobox-export",
                            "--list-binaries",
                        ],
                        list_output,
                    )
                    // No cat command registered, so it will fail to read the wrapper
                    .build(),
                default_cmd_factory(),
            );
            let binaries = db.get_exported_binaries("test-box").await?;
            assert_eq!(binaries.len(), 1);
            // Should fallback to extracting name from exported_path
            assert_eq!(binaries[0].name, "my-tool");
            // source_path will be same as exported_path when wrapper can't be read
            assert_eq!(binaries[0].source_path, "/home/user/.local/bin/my-tool");
            assert_eq!(binaries[0].exported_path, "/home/user/.local/bin/my-tool");
            Ok(())
        })
    }
}
