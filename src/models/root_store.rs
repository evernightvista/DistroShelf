use anyhow::Context;
use futures::prelude::*;
use glib::Properties;
use glib::subclass::prelude::*;
use gtk::glib::clone;
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::OnceCell;
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;
use tracing::error;
use tracing::info;
use tracing::{debug, warn};

use crate::backends::Distrobox;
use crate::backends::Status;
use crate::backends::container_runtime::{DetectedRuntime, get_container_runtime};
use crate::backends::supported_terminals::{Terminal, TerminalRepository};
use crate::backends::{self, CreateArgs, ExportableApp};
use crate::distrobox_init_migration::domain::{StaleContainer, current_init_path};
use crate::distrobox_init_migration::migrate_stale_path;
use crate::fakers::{Command, CommandRunner, FdMode, FileSystem, Settings};
use crate::gtk_utils::TypedListStore;
use crate::models::Container;
use crate::models::ContainerSortKey;
use crate::models::DistroboxExecutable;
use crate::models::DistroboxSource;
use crate::models::DistroboxTask;
use crate::models::MainStore;
use crate::models::VersionedExecutable;
use crate::models::ViewType;
use crate::models::{DialogParams, DialogType};
use crate::query::Query;

const SHORTCUT_DEFINITIONS: [(&str, &str); 13] = [
    ("<primary>q", "app.quit"),
    ("<primary>question", "app.shortcuts"),
    ("F5", "win.refresh"),
    ("<primary>u", "win.upgrade-container"),
    ("<primary><shift>u", "win.upgrade-all"),
    ("<primary>i", "win.install-package"),
    ("<primary>comma", "win.preferences"),
    ("<primary>period", "win.open-terminal"),
    ("<primary>e", "win.view-exportable-apps"),
    ("<primary>Delete", "win.delete-container"),
    ("<primary>s", "win.stop-container"),
    ("<primary>l", "win.command-log"),
    ("<primary>d", "win.delete-container"),
];

mod imp {
    use crate::{
        backends::container_runtime::DetectedRuntime, models::VersionedExecutable, query::Query,
    };

    use super::*;

    #[derive(Properties)]
    #[properties(wrapper_type = super::RootStore)]
    pub struct RootStore {
        pub terminal_repository: RefCell<TerminalRepository>,
        pub command_runner: OnceCell<CommandRunner>,
        pub container_runtime: Query<DetectedRuntime>,

        pub distrobox_version: RefCell<Query<Option<DistroboxExecutable>>>,
        pub bundled_distrobox_version: Query<Option<VersionedExecutable>>,
        pub host_distrobox_version: Query<Option<VersionedExecutable>>,

        pub tasks: TypedListStore<DistroboxTask>,
        #[property(get, set, nullable)]
        pub selected_task: RefCell<Option<DistroboxTask>>,

        /// The active main-view state. `None` when Welcome is showing.
        #[property(get, set = Self::set_main_store, nullable)]
        pub main_store: RefCell<Option<MainStore>>,

        pub settings: RefCell<Settings>,
        pub file_system: RefCell<FileSystem>,

        pub shortcuts: gio::ListStore,
        pub shortcuts_enabled: std::cell::Cell<bool>,

        #[property(get, set = Self::set_current_view, builder(ViewType::default()))]
        current_view: RefCell<ViewType>,
        #[property(get, set, builder(DialogType::default()))]
        current_dialog: RefCell<DialogType>,

        #[property(get, set)]
        bundled_update_available: std::cell::Cell<bool>,

        /// Parameters for the current dialog (not a GObject property)
        pub dialog_params: RefCell<DialogParams>,

        /// Guards against concurrent `ensure_selected_terminal_after_load` spawns.
        pub terminal_selection_in_flight: std::cell::Cell<bool>,
    }

    impl Default for RootStore {
        fn default() -> Self {
            Self {
                command_runner: OnceCell::new(),
                container_runtime: Query::new("container_runtime".into(), || async {
                    anyhow::bail!("Container runtime not initialized")
                }),
                terminal_repository: RefCell::new(TerminalRepository::new(
                    CommandRunner::new_null(),
                    FileSystem::new_null(),
                )),
                current_view: Default::default(),
                current_dialog: Default::default(),
                dialog_params: Default::default(),
                distrobox_version: RefCell::new(Query::new("distrobox_version".into(), || async {
                    Ok(None)
                })),
                bundled_distrobox_version: Query::new(
                    "bundled_distrobox_version".into(),
                    || async { Ok(None) },
                ),
                host_distrobox_version: Query::new("host_distrobox_version".into(), || async {
                    Ok(None)
                }),
                tasks: TypedListStore::new(),
                selected_task: Default::default(),
                bundled_update_available: std::cell::Cell::new(false),
                settings: RefCell::new(Settings::new_null()),
                file_system: RefCell::new(FileSystem::new_null()),
                shortcuts: gio::ListStore::new::<gtk::Shortcut>(),
                shortcuts_enabled: std::cell::Cell::new(false),
                main_store: RefCell::new(None),
                terminal_selection_in_flight: std::cell::Cell::new(false),
            }
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for RootStore {
        fn constructed(&self) {
            self.parent_constructed();
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RootStore {
        const NAME: &'static str = "RootStore";
        type Type = super::RootStore;
    }

    impl RootStore {
        fn set_main_store(&self, value: Option<MainStore>) {
            self.main_store.replace(value);
        }

        fn set_current_view(&self, value: ViewType) {
            let obj = self.obj();
            if *self.current_view.borrow() == value {
                return;
            }
            *self.current_view.borrow_mut() = value;
            match value {
                ViewType::Main => {
                    if self.main_store.borrow().is_none() {
                        let settings = obj.settings();
                        let sort_key = ContainerSortKey::from_str(&settings.string("sort-key"))
                            .unwrap_or_default();
                        let main = MainStore::new(
                            obj.command_runner(),
                            obj.container_runtime(),
                            obj.distrobox_version(),
                            sort_key,
                            settings,
                        );
                        *self.main_store.borrow_mut() = Some(main.clone());
                        // A store created for this view switch starts empty
                        // (e.g. entering Main after completing the welcome
                        // flow): populate it right away.
                        main.load_containers();
                    }
                }
                ViewType::Welcome => {
                    *self.main_store.borrow_mut() = None;
                }
            }
            obj.notify("current-view");
            obj.notify("main-store");
        }
    }
}

glib::wrapper! {
    pub struct RootStore(ObjectSubclass<imp::RootStore>);
}

#[derive(Debug, Clone)]
enum SelectedTerminalResolution {
    Empty,
    Found(Terminal),
    Missing(String),
}

impl RootStore {
    pub fn new(command_runner: CommandRunner, settings: Settings, file_system: FileSystem) -> Self {
        let this: Self = glib::Object::builder().build();

        this.imp()
            .command_runner
            .set(command_runner.clone())
            .or(Err("command_runner already set"))
            .unwrap();

        this.imp().settings.replace(settings);
        this.imp().file_system.replace(file_system.clone());

        this.imp()
            .terminal_repository
            .replace(TerminalRepository::new(command_runner.clone(), file_system));

        let this_clone = this.clone();
        this.terminal_repository()
            .flatpak_terminals_query()
            .connect_success(move |_terminals| {
                this_clone.ensure_selected_terminal_after_load();
            });

        let this_clone = this.clone();
        this.terminal_repository()
            .flatpak_terminals_query()
            .connect_error(move |_error| {
                this_clone.ensure_selected_terminal_after_load();
            });

        let this_clone = this.clone();
        this.terminal_repository()
            .json_terminals_query()
            .connect_success(move |_terminals| {
                this_clone.ensure_selected_terminal_after_load();
            });

        let this_clone = this.clone();
        this.terminal_repository()
            .json_terminals_query()
            .connect_error(move |_error| {
                this_clone.ensure_selected_terminal_after_load();
            });

        // selected_source drives the distrobox_version derivation.
        let this_clone_for_source = this.clone();
        let selected_source = Query::<DistroboxSource>::new("selected_source".into(), move || {
            let this = this_clone_for_source.clone();
            async move { Ok(DistroboxSource::from_setting(&this.settings())) }
        });

        let host_version = this.host_distrobox_version().switch_map(|info| match info {
            Some(exe) => Query::pure(Some(DistroboxExecutable::Host(exe.clone()))),
            None => Query::pure(None),
        });

        let bundled_version = this
            .bundled_distrobox_version()
            .switch_map(|info| match info {
                Some(exe) => Query::pure(Some(DistroboxExecutable::Bundled(exe.clone()))),
                None => Query::pure(None),
            });

        let distrobox_version = selected_source.switch_map({
            let host_version = host_version.clone();
            let bundled_version = bundled_version.clone();
            move |source: &DistroboxSource| match source {
                DistroboxSource::Host => host_version.clone(),
                DistroboxSource::Bundled => bundled_version.clone(),
            }
        });

        this.imp().distrobox_version.replace(distrobox_version);

        let this_clone = this.clone();
        this.distrobox_version().connect_success(move |exe| {
            if exe.is_none() {
                this_clone.set_current_view(ViewType::Welcome);
                debug_assert_eq!(
                    this_clone.current_view(),
                    ViewType::Welcome,
                    "view must be Welcome after distrobox resolves to None"
                );
            } else {
                let source = this_clone.distrobox_source();
                debug_assert!(
                    matches!(
                        (&exe, source),
                        (Some(DistroboxExecutable::Host(_)), DistroboxSource::Host)
                            | (
                                Some(DistroboxExecutable::Bundled(_)),
                                DistroboxSource::Bundled
                            )
                    ),
                    "distrobox_version variant must match selected_source"
                );
            }
            this_clone.update_bundled_update_available();
            if let Some(main) = this_clone.main_store() {
                main.check_stale_containers();
            }
        });
        let this_clone = this.clone();
        this.distrobox_version().connect_error(move |_error| {
            this_clone.set_current_view(ViewType::Welcome);
            this_clone.update_bundled_update_available();
        });

        let this_clone = this.clone();
        this.container_runtime().connect_success(move |_runtime| {
            if let Some(main) = this_clone.main_store() {
                main.check_stale_containers();
                main.load_containers();
            }
        });

        let this_clone = this.clone();
        this.imp().host_distrobox_version.set_fetcher(move || {
            let command_runner = this_clone.command_runner();
            async move {
                let mut version_cmd = Command::new("distrobox");
                version_cmd.arg("version");
                let version = command_runner
                    .output(version_cmd)
                    .await
                    .ok()
                    .filter(|o| o.status.success())
                    .and_then(|o| {
                        let text = String::from_utf8_lossy(&o.stdout).to_string();
                        text.split(':').nth(1).map(|s| s.trim().to_string())
                    });

                let mut path_cmd = Command::new("sh");
                path_cmd.arg("-c").arg("command -v distrobox");
                let path = command_runner
                    .output(path_cmd)
                    .await
                    .ok()
                    .filter(|o| o.status.success())
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                    .filter(|s| !s.is_empty());

                Ok(match (version, path) {
                    (Some(version), Some(path)) => Some(VersionedExecutable { version, path }),
                    _ => None,
                })
            }
        });

        let this_clone = this.clone();
        this.imp().bundled_distrobox_version.set_fetcher(move || {
            let this_clone = this_clone.clone();
            async move {
                let Some(path) = crate::distrobox_downloader::resolve_bundled_distrobox_path(
                    &this_clone.file_system(),
                ) else {
                    return Ok(None);
                };
                let path = path.to_string_lossy().into_owned();
                let temp_factory: crate::backends::distrobox::command::CmdFactory = Rc::new({
                    let path = path.clone();
                    move || Command::new(path.clone())
                });
                let distrobox = Distrobox::new(this_clone.command_runner(), temp_factory);
                let version = distrobox.version().map_err(anyhow::Error::from).await?;
                Ok(Some(VersionedExecutable { version, path }))
            }
        });

        let this_clone = this.clone();
        this.imp().container_runtime.set_fetcher(move || {
            let this_clone = this_clone.clone();
            async move {
                get_container_runtime(this_clone.command_runner())
                    .await
                    .ok_or_else(|| anyhow::anyhow!("No container runtime available"))
            }
        });

        // Settings change handler
        {
            let selected_source_clone = selected_source.clone();
            this.settings().connect_changed(
                Some("distrobox-executable"),
                clone!(
                    #[weak(rename_to = obj)]
                    this,
                    #[strong]
                    selected_source_clone,
                    move |_key| {
                        let new_source = DistroboxSource::from_setting(&obj.settings());
                        selected_source_clone.refetch();
                        if new_source == DistroboxSource::Bundled {
                            let obj_clone = obj.clone();
                            glib::spawn_future_local(async move {
                                if crate::distrobox_downloader::resolve_bundled_distrobox_path(
                                    &obj_clone.file_system(),
                                )
                                .is_none()
                                {
                                    obj_clone.download_distrobox();
                                } else {
                                    obj_clone.bundled_distrobox_version().refetch();
                                }
                            });
                        } else {
                            obj.host_distrobox_version().refetch();
                        }
                        obj.update_bundled_update_available();
                    }
                ),
            );
        }

        this.enable_shortcuts();

        // Create MainStore and set initial view to Main (optimistic start)
        let settings = this.settings();
        let sort_key = ContainerSortKey::from_str(&settings.string("sort-key")).unwrap_or_default();
        let main_store = MainStore::new(
            command_runner.clone(),
            this.container_runtime(),
            this.distrobox_version(),
            sort_key,
            settings,
        );
        *this.imp().main_store.borrow_mut() = Some(main_store);
        this.set_current_view(ViewType::Main);
        // set_current_view returned early because the default view is already
        // Main. Notify manually so listeners wired after construction pick up
        // the initial state.
        this.notify("main-store");

        this
    }

    fn build_shortcut(trigger: &str, action: &str) -> Option<gtk::Shortcut> {
        let trigger =
            gtk::ShortcutTrigger::parse_string(trigger).expect("Invalid shortcut trigger");
        let action = gtk::NamedAction::new(action);
        Some(gtk::Shortcut::new(Some(trigger), Some(action)))
    }

    fn rebuild_shortcuts_model(&self) {
        let shortcuts = &self.imp().shortcuts;
        shortcuts.remove_all();
        for (trigger, action) in SHORTCUT_DEFINITIONS {
            if let Some(shortcut) = Self::build_shortcut(trigger, action) {
                shortcuts.append(&shortcut);
            }
        }
    }

    pub fn shortcuts_model(&self) -> gio::ListStore {
        self.imp().shortcuts.clone()
    }

    pub fn shortcut_action_names(&self) -> Vec<&'static str> {
        SHORTCUT_DEFINITIONS
            .iter()
            .map(|(_, action)| *action)
            .collect::<Vec<_>>()
    }

    pub fn enable_shortcuts(&self) {
        if self.imp().shortcuts_enabled.get() {
            return;
        }

        self.rebuild_shortcuts_model();
        self.imp().shortcuts_enabled.set(true);
    }

    pub fn disable_shortcuts(&self) {
        if !self.imp().shortcuts_enabled.get() {
            return;
        }

        self.imp().shortcuts.remove_all();
        self.imp().shortcuts_enabled.set(false);
    }

    pub fn start_background_tasks(&self) {
        self.distrobox_version().refetch();
        self.host_distrobox_version().refetch();
        self.bundled_distrobox_version().refetch();
        self.container_runtime().refetch();
        self.terminal_repository().load_all();

        if let Some(main) = self.main_store() {
            main.start_listening_podman_events();
        }
    }

    fn selected_terminal_setting_is_empty(&self) -> bool {
        let selected_terminal: String = self.settings().string("selected-terminal");
        selected_terminal.is_empty()
    }

    fn terminal_sources_loading(&self) -> bool {
        let terminal_repository = self.terminal_repository();
        terminal_repository.json_terminals_query().is_loading()
            || terminal_repository.flatpak_terminals_query().is_loading()
    }

    fn selected_terminal_resolution(&self) -> SelectedTerminalResolution {
        let name_or_program: String = self.settings().string("selected-terminal");
        if name_or_program.is_empty() {
            return SelectedTerminalResolution::Empty;
        }

        let terminal_repository = self.terminal_repository();

        terminal_repository
            .terminal_by_name(&name_or_program)
            .or_else(|| terminal_repository.terminal_by_program(&name_or_program))
            .map(SelectedTerminalResolution::Found)
            .unwrap_or_else(|| SelectedTerminalResolution::Missing(name_or_program))
    }

    fn ensure_selected_terminal_after_load(&self) {
        if self.selected_terminal().is_some() {
            return;
        }

        let terminal_repository = self.terminal_repository();
        if terminal_repository.json_terminals_query().is_loading()
            || terminal_repository.flatpak_terminals_query().is_loading()
        {
            return;
        }

        if self.imp().terminal_selection_in_flight.replace(true) {
            return;
        }

        let this = self.clone();
        glib::MainContext::ref_thread_default().spawn_local(async move {
            let Some(default_terminal) = this.terminal_repository().default_terminal().await else {
                this.imp().terminal_selection_in_flight.set(false);
                return;
            };
            if this.selected_terminal().is_none() && this.selected_terminal_setting_is_empty() {
                this.set_selected_terminal_name(&default_terminal.name);
            }
            this.imp().terminal_selection_in_flight.set(false);
        });
    }

    pub fn distrobox_version(&self) -> Query<Option<DistroboxExecutable>> {
        self.imp().distrobox_version.borrow().clone()
    }

    pub fn bundled_distrobox_version(&self) -> Query<Option<VersionedExecutable>> {
        self.imp().bundled_distrobox_version.clone()
    }

    pub fn host_distrobox_version(&self) -> Query<Option<VersionedExecutable>> {
        self.imp().host_distrobox_version.clone()
    }

    pub fn container_runtime(&self) -> Query<DetectedRuntime> {
        self.imp().container_runtime.clone()
    }

    pub fn command_runner(&self) -> CommandRunner {
        self.imp().command_runner.get().unwrap().clone()
    }

    pub fn settings(&self) -> Settings {
        self.imp().settings.borrow().clone()
    }

    pub fn file_system(&self) -> FileSystem {
        self.imp().file_system.borrow().clone()
    }

    pub fn terminal_repository(&self) -> TerminalRepository {
        self.imp().terminal_repository.borrow().clone()
    }

    pub fn tasks(&self) -> &TypedListStore<DistroboxTask> {
        &self.imp().tasks
    }

    // --- Delegation to MainStore ---

    pub fn selected_container_model(&self) -> Option<gtk::SingleSelection> {
        self.main_store().map(|m| m.selected_container_model())
    }

    pub fn selected_container(&self) -> Option<Container> {
        self.main_store().and_then(|m| m.selected_container())
    }

    pub fn selected_container_name(&self) -> Option<String> {
        self.main_store().and_then(|m| m.selected_container_name())
    }

    pub fn load_containers(&self) {
        if let Some(main) = self.main_store() {
            main.load_containers();
        }
    }

    pub fn containers(&self) -> Option<TypedListStore<Container>> {
        self.main_store().map(|m| m.containers().clone())
    }

    pub fn check_stale_containers(&self) {
        if let Some(main) = self.main_store() {
            main.check_stale_containers();
        }
    }

    pub fn stale_containers(&self) -> Option<TypedListStore<glib::BoxedAnyObject>> {
        self.main_store().map(|m| m.stale_containers().clone())
    }

    pub fn images_query(&self) -> Option<Query<Vec<String>>> {
        self.main_store().map(|m| m.images_query())
    }

    pub fn downloaded_images_query(&self) -> Option<Query<HashSet<String>>> {
        self.main_store().map(|m| m.downloaded_images_query())
    }

    pub fn containers_query(&self) -> Option<Query<Vec<Container>>> {
        self.main_store().map(|m| m.containers_query())
    }

    /// Repairs all containers in `stale_containers()` by symlinking their
    /// stale `distrobox-init` paths to the current one. Running containers
    /// are skipped (stop them and re-run). Returns the task so the caller
    /// can display it.
    pub fn migrate_stale_containers(&self) -> Option<DistroboxTask> {
        let main = self.main_store()?;

        // Guard: if a migration task is already in progress, return it
        for task in self.tasks().iter() {
            if task.name() == "migrate-init" && !task.ended() {
                return Some(task);
            }
        }

        let stale: Vec<StaleContainer> = main
            .stale_containers()
            .iter()
            .map(|obj| obj.borrow::<StaleContainer>().clone())
            .collect();

        let this = self.clone();
        Some(
            self.create_task("system", "migrate-init", move |task| async move {
                task.set_description(
                    "Repairing containers that point to an outdated distrobox-init location",
                );
                let Some(Some(exe)) = this.distrobox_version().data() else {
                    anyhow::bail!("No distrobox executable available");
                };
                let Some(current_init) = current_init_path(exe.path()) else {
                    anyhow::bail!(
                        "Cannot determine the distrobox-init location from {}",
                        exe.path()
                    );
                };

                let distrobox = this
                    .main_store()
                    .expect("migrate_stale_containers requires Main view")
                    .distrobox()
                    .clone();
                let running: HashSet<String> = distrobox
                    .list()
                    .await?
                    .into_values()
                    .filter(|c| matches!(c.status, Status::Up(_)))
                    .map(|c| c.name)
                    .collect();

                let runner = this.command_runner();
                let mut failed = 0;
                let mut skipped = 0;
                for entry in &stale {
                    if running.contains(&entry.name) {
                        task.append_output(&format!(
                            "Skipping {}: the container is running. Stop it and migrate again.\r\n",
                            entry.name
                        ));
                        skipped += 1;
                        continue;
                    }
                    task.append_output(&format!(
                        "{}: linking scripts in {} -> {}\r\n",
                        entry.name,
                        entry
                            .stale_init_path
                            .parent()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default(),
                        current_init
                            .parent()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default(),
                    ));
                    match migrate_stale_path(&runner, &entry.stale_init_path, &current_init).await {
                        Ok(()) => {
                            task.append_output(&format!(
                                "{}: migrated successfully\r\n",
                                entry.name
                            ));
                        }
                        Err(e) => {
                            task.append_output(&format!("{}: failed: {}\r\n", entry.name, e));
                            failed += 1;
                        }
                    }
                }

                if let Some(main) = this.main_store() {
                    main.check_stale_containers();
                }
                if failed > 0 {
                    anyhow::bail!("Failed to migrate {} container(s)", failed);
                }
                if skipped > 0 {
                    task.append_output(&format!(
                        "Skipped {} running container(s). Stop them and migrate again.\r\n",
                        skipped
                    ));
                }
                Ok(())
            }),
        )
    }

    pub fn distrobox_source(&self) -> DistroboxSource {
        DistroboxSource::from_setting(&self.settings())
    }

    pub fn is_distrobox_bundled(&self) -> bool {
        self.distrobox_source() == DistroboxSource::Bundled
    }

    pub fn set_distrobox_source(&self, source: DistroboxSource) {
        self.settings()
            .set_string("distrobox-executable", source.to_setting_str())
            .expect("distrobox-executable key must exist in schema");
    }

    pub fn update_bundled_update_available(&self) {
        if self.distrobox_source() == DistroboxSource::Bundled
            && let Some(Some(installed)) = self.bundled_distrobox_version().data()
        {
            let available =
                crate::distrobox_downloader::is_bundled_update_available(&installed.version);
            self.set_bundled_update_available(available);
            return;
        }
        self.set_bundled_update_available(false);
    }

    pub fn download_distrobox(&self) -> DistroboxTask {
        for task in self.tasks().iter() {
            if task.name() == "Downloading Distrobox" && !task.ended() {
                return task;
            }
        }
        let root_store_weak = self.downgrade();
        let task = self.create_task("system", "Downloading Distrobox", move |task| async move {
            crate::distrobox_downloader::download_distrobox(task, root_store_weak).await
        });
        self.set_selected_task(Some(task.clone()));
        task
    }

    pub fn create_task<F, Fut>(&self, name: &str, action: &str, operation: F) -> DistroboxTask
    where
        F: FnOnce(DistroboxTask) -> Fut + 'static,
        Fut: std::future::Future<Output = Result<(), anyhow::Error>> + 'static,
    {
        let this = self.clone();
        info!("Creating new distrobox task");
        let name = name.to_string();
        let action = action.to_string();

        let task = DistroboxTask::new(&name, &action, move |task| async move {
            debug!("Starting task execution");
            let result = operation(task).await;
            if let Err(ref e) = result {
                error!(error = %e, "Task execution failed");
            }
            if let Some(main) = this.main_store() {
                main.load_containers();
            }
            result
        });

        self.tasks().append(&task);
        task
    }

    pub fn clear_ended_tasks(&self) {
        self.tasks().retain(|task| !task.ended());
    }

    pub fn create_container(&self, create_args: CreateArgs) {
        let distrobox = self
            .main_store()
            .expect("create_container requires Main view")
            .distrobox()
            .clone();
        let name = create_args.name.to_string();
        let task = self.create_task(&name, "create", move |task| async move {
            task.set_description(
                "Creation requires downloading the container image, which may take some time...",
            );
            let child = distrobox.create(create_args).await?;
            task.handle_child_output(child).await
        });
        self.view_task(&task);
    }
    pub fn clone_container(&self, source_name: &str, create_args: CreateArgs) {
        let distrobox = self
            .main_store()
            .expect("clone_container requires Main view")
            .distrobox()
            .clone();
        let name = create_args.name.to_string();
        let source = source_name.to_string();
        let task = self.create_task(&name, "clone", move |task| {
            let distrobox = distrobox.clone();
            let create_args = create_args;
            let source = source.clone();
            async move {
                task.set_description("Cloning container (may take some time)...");
                let child = distrobox.clone_from(&source, create_args).await?;
                task.handle_child_output(child).await
            }
        });
        self.view_task(&task);
    }
    pub fn assemble_container(&self, file_path: &str) {
        let distrobox = self
            .main_store()
            .expect("assemble_container requires Main view")
            .distrobox()
            .clone();
        let file_path_clone = file_path.to_string();
        let file_name = Path::new(file_path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(file_path);

        let task = self.create_task(file_name, "assemble", move |task| async move {
            let child = distrobox.assemble(&file_path_clone)?;
            task.handle_child_output(child).await
        });
        self.view_task(&task);
    }

    pub fn upgrade_container(&self, container: &Container) -> DistroboxTask {
        let distrobox = self
            .main_store()
            .expect("upgrade_container requires Main view")
            .distrobox()
            .clone();
        let name_for_task = container.name();
        let name = name_for_task.clone();
        self.create_task(&name_for_task, "upgrade", move |task| async move {
            let child = distrobox.upgrade(&name)?;
            task.handle_child_output(child).await
        })
    }

    pub fn launch_app(&self, container: &Container, app: ExportableApp) {
        let distrobox = self
            .main_store()
            .expect("launch_app requires Main view")
            .distrobox()
            .clone();
        let container = container.clone();
        self.create_task(&container.name(), "launch-app", move |task| async move {
            let child = distrobox.launch_app(&container.name(), &app)?;
            task.handle_child_output(child).await
        });
    }

    pub fn install_package(&self, container: &Container, path: &Path) {
        let Some(distro) = container.distro() else {
            tracing::error!(
                container = %container.name(),
                "Cannot install package: distro information not available"
            );
            return;
        };

        let distrobox = self
            .main_store()
            .expect("install_package requires Main view")
            .distrobox()
            .clone();
        let this = self.clone();
        let package_manager = distro.package_manager();
        let path_clone = path.to_owned();
        let name_for_task = container.name();
        let name = name_for_task.clone();
        self.create_task(&name_for_task, "install", move |task| async move {
            task.set_description(format!("Installing {:?}", path_clone));
            // The file provided from the portal is under /run/user/1000 which
            // is not accessible by root. Copy the file as a normal user to
            // /tmp first, then install.
            let enter_cmd = distrobox.enter_cmd(&name);

            // The file must have the correct extension (e.g. .deb for apt-get).
            // Use the original filename or generate one with the proper extension.
            let filename = path_clone
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.to_string())
                .unwrap_or_else(|| {
                    format!(
                        "package{}",
                        package_manager.installable_file().unwrap_or("")
                    )
                });
            let tmp_path = format!("/tmp/com.ranfdev.DistroShelf.{}", filename);
            let tmp_path = Path::new(&tmp_path);
            let cp_cmd_pure = Command::new_with_args("cp", [&path_clone, tmp_path]);

            let Some(install_cmd_pure) = package_manager.install_cmd(tmp_path) else {
                anyhow::bail!(
                    "Package manager {:?} does not support installing files",
                    package_manager
                );
            };

            let mut cp_cmd = enter_cmd.clone();
            cp_cmd.extend("--", &cp_cmd_pure);
            let mut install_cmd = enter_cmd.clone();
            install_cmd.extend("--", &install_cmd_pure);

            this.spawn_terminal_cmd(name.clone(), &cp_cmd).await?;
            this.spawn_terminal_cmd(name, &install_cmd).await
        });
    }

    pub fn export_app(&self, container: &Container, desktop_file_path: &str) {
        let distrobox = self
            .main_store()
            .expect("export_app requires Main view")
            .distrobox()
            .clone();
        let container = container.clone();
        let desktop_file_path = desktop_file_path.to_string();
        self.create_task(&container.name(), "export", move |_task| async move {
            distrobox
                .export_app(&container.name(), &desktop_file_path)
                .await?;
            container.apps().refetch();
            Ok(())
        });
    }

    pub fn unexport_app(&self, container: &Container, desktop_file_path: &str) {
        let distrobox = self
            .main_store()
            .expect("unexport_app requires Main view")
            .distrobox()
            .clone();
        let container = container.clone();
        let desktop_file_path = desktop_file_path.to_string();
        self.create_task(&container.name(), "unexport", move |_task| async move {
            distrobox
                .unexport_app(&container.name(), &desktop_file_path)
                .await?;
            container.apps().refetch();
            Ok(())
        });
    }

    pub fn export_binary(&self, container: &Container, binary_path: &str) -> DistroboxTask {
        let distrobox = self
            .main_store()
            .expect("export_binary requires Main view")
            .distrobox()
            .clone();
        let container = container.clone();
        let binary_path = binary_path.to_string();
        self.create_task(
            &container.name(),
            "export-binary",
            move |_task| async move {
                distrobox
                    .export_binary(&container.name(), &binary_path)
                    .await?;
                container.binaries().refetch();
                Ok(())
            },
        )
    }

    pub fn unexport_binary(&self, container: &Container, binary_path: &str) {
        let distrobox = self
            .main_store()
            .expect("unexport_binary requires Main view")
            .distrobox()
            .clone();
        let container = container.clone();
        let binary_path = binary_path.to_string();
        self.create_task(
            &container.name(),
            "unexport-binary",
            move |_task| async move {
                distrobox
                    .unexport_binary(&container.name(), &binary_path)
                    .await?;
                container.binaries().refetch();
                Ok(())
            },
        );
    }

    pub fn delete_container(&self, container: &Container) {
        let distrobox = self
            .main_store()
            .expect("delete_container requires Main view")
            .distrobox()
            .clone();
        let name_for_task = container.name();
        let name = name_for_task.clone();
        self.create_task(&name_for_task, "delete", move |_task| async move {
            distrobox.remove(&name).await?;
            Ok(())
        });
    }

    pub fn stop_container(&self, container: &Container) {
        let distrobox = self
            .main_store()
            .expect("stop_container requires Main view")
            .distrobox()
            .clone();
        let name_for_task = container.name();
        let name = name_for_task.clone();
        self.create_task(&name_for_task, "stop", move |_task| async move {
            distrobox.stop(&name).await?;
            Ok(())
        });
    }

    pub fn spawn_container_terminal(&self, container: &Container) -> DistroboxTask {
        let distrobox = self
            .main_store()
            .expect("spawn_container_terminal requires Main view")
            .distrobox()
            .clone();
        let this = self.clone();
        let name_for_task = container.name();
        let name = name_for_task.clone();
        self.create_task(&name_for_task, "spawn-terminal", move |_task| async move {
            let enter_cmd = distrobox.enter_cmd(&name);
            this.spawn_terminal_cmd(name, &enter_cmd).await
        })
    }

    pub fn upgrade_all(&self) {
        if let Some(main) = self.main_store() {
            let containers = main.all_containers();
            for container in containers {
                self.upgrade_container(&container);
            }
        }
    }

    pub fn view_task(&self, task: &DistroboxTask) {
        self.set_selected_task(Some(task));
        self.set_current_dialog(DialogType::TaskManager);
    }
    pub fn view_exportable_apps(&self) {
        let this = self.clone();
        this.set_current_dialog(DialogType::ExportableApps);
    }

    pub fn open_dialog(&self, dialog_type: DialogType, params: DialogParams) {
        self.imp().dialog_params.replace(params);
        self.set_current_dialog(dialog_type);
    }

    pub fn dialog_params(&self) -> std::cell::Ref<'_, DialogParams> {
        self.imp().dialog_params.borrow()
    }

    pub fn take_dialog_params(&self) -> DialogParams {
        self.imp().dialog_params.take()
    }

    pub async fn spawn_terminal_cmd(
        &self,
        name: String,
        cmd: &Command,
    ) -> Result<(), anyhow::Error> {
        let supported_terminal = match self.selected_terminal_resolution() {
            SelectedTerminalResolution::Found(terminal) => terminal,
            SelectedTerminalResolution::Empty => {
                error!("No terminal selected when trying to spawn terminal");
                return Err(anyhow::anyhow!("No terminal selected"));
            }
            SelectedTerminalResolution::Missing(name_or_program) => {
                if !self.terminal_sources_loading() {
                    error!("Terminal not found: {}", name_or_program);
                }
                return Err(anyhow::anyhow!(
                    "Selected terminal '{}' not found",
                    name_or_program
                ));
            }
        };
        let mut spawn_cmd = Command::new(supported_terminal.program);
        spawn_cmd
            .args(supported_terminal.extra_args)
            .arg(supported_terminal.separator_arg)
            .arg(cmd.program.clone())
            .args(cmd.args.clone());

        debug!(?spawn_cmd, "Spawning terminal command");
        let mut child = self.command_runner().spawn(spawn_cmd)?;

        let this = self.clone();
        glib::MainContext::ref_thread_default().spawn_local(async move {
            this.reload_till_up(name, 5);
        });
        if !child.wait().await?.success() {
            return Err(anyhow::anyhow!("Failed to spawn terminal"));
        }
        Ok(())
    }

    pub fn selected_terminal(&self) -> Option<Terminal> {
        match self.selected_terminal_resolution() {
            SelectedTerminalResolution::Found(terminal) => Some(terminal),
            SelectedTerminalResolution::Empty | SelectedTerminalResolution::Missing(_) => None,
        }
    }
    pub fn set_selected_terminal_name(&self, name: &str) {
        self.settings()
            .set_string("selected-terminal", name)
            .expect("Failed to save setting");
    }

    pub async fn validate_terminal(&self) -> Result<(), anyhow::Error> {
        let terminal = match self.selected_terminal_resolution() {
            SelectedTerminalResolution::Found(terminal) => terminal,
            SelectedTerminalResolution::Empty => {
                error!("No terminal selected for validation");
                return Err(anyhow::anyhow!("No terminal selected"));
            }
            SelectedTerminalResolution::Missing(name_or_program) => {
                if !self.terminal_sources_loading() {
                    error!("Terminal not found: {}", name_or_program);
                }
                return Err(anyhow::anyhow!(
                    "Selected terminal '{}' not found",
                    name_or_program
                ));
            }
        };
        info!(terminal = %terminal.program, "Validating terminal");

        let mut cmd = Command::new(terminal.program.clone());
        cmd.args(terminal.extra_args.clone())
            .arg(terminal.separator_arg)
            .arg("echo")
            .arg("DistroShelf terminal validation");

        let mut child = match self.command_runner().spawn(cmd) {
            Ok(child) => child,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                error!(terminal = %terminal.program, "Terminal program not found");
                return Err(anyhow::anyhow!(
                    "Terminal program '{}' not found. Please install it or choose a different terminal.",
                    &terminal.program
                ));
            }
            Err(e) => return Err(e.into()),
        };

        if !child.wait().await?.success() {
            error!(terminal = %terminal.program, "Terminal validation failed");
            return Err(anyhow::anyhow!(
                "Terminal validation failed. '{}' did not run successfully.",
                &terminal.program
            ));
        }

        Ok(())
    }
    fn reload_till_up(&self, name: String, times: usize) {
        let this = self.clone();
        glib::MainContext::ref_thread_default().spawn_local(async move {
            let distrobox = this
                .main_store()
                .expect("reload_till_up requires Main view")
                .distrobox()
                .clone();
            for i in 1..times {
                glib::timeout_future(Duration::from_millis(i as u64 * 300)).await;

                let containers = match distrobox.list().await {
                    Ok(c) => c,
                    Err(e) => {
                        warn!(error = %e, "Failed to list containers while waiting for container to start");
                        continue;
                    }
                };
                let Some(container) = containers.get(&name) else {
                    debug!(name = %name, "Container not found while waiting for it to start");
                    continue;
                };

                if let Status::Up(_) = &container.status {
                    this.load_containers();
                    return;
                }
            }
        });
    }

    pub async fn run_to_string(&self, mut cmd: Command) -> Result<String, anyhow::Error> {
        cmd.stderr = FdMode::Pipe;
        cmd.stdout = FdMode::Pipe;
        let output = self.command_runner().output(cmd.clone()).await?;
        Ok(String::from_utf8(output.stdout).map_err(|e| {
            error!(cmd = %cmd, "Failed to parse command output");
            backends::Error::ParseOutput(e.to_string())
        })?)
    }

    pub async fn is_nvidia_host(&self) -> bool {
        debug!("Checking if host is NVIDIA");
        let cmd = Command::new("lspci");
        let output = glib::future_with_timeout(Duration::from_secs(2), async move {
            self.run_to_string(cmd).await.context("Calling lspci")
        })
        .await
        .context("timeout")
        .flatten();
        match output {
            Ok(output) => {
                let is_nvidia = output.contains("NVIDIA") || output.contains("nVidia");
                debug!(is_nvidia, "lspci ran successfully");
                is_nvidia
            }
            Err(e) => {
                warn!(?e, "Failed to check if host is NVIDIA");
                false
            }
        }
    }

    fn getfattr_cmd(path: &str) -> Command {
        Command::new_with_args(
            "getfattr",
            [
                "-n",
                "user.document-portal.host-path",
                "--only-values",
                path,
            ],
        )
    }

    pub async fn resolve_host_path(&self, path: &str) -> Result<String, backends::Error> {
        // The path could be a host path already resolved to a real location
        // (e.g. /home/user/Documents) or a flatpak sandbox path
        // (e.g. /run/user/1000/doc/abc123). Use getfattr to resolve sandbox
        // paths via the document portal's xattr. If getfattr returns empty
        // output, the path is already a real host path.
        debug!(?path, "Resolving host path");

        let cmd = Self::getfattr_cmd(path);
        let output = self
            .run_to_string(cmd)
            .await
            .map_err(|e| backends::Error::ResolveHostPath(e.to_string()));

        let is_from_sandbox = path.starts_with("/run/user");

        match output {
            Ok(resolved_path) => {
                debug!(?resolved_path, "Resolved host path");
                if resolved_path.is_empty() {
                    return Ok(path.to_string());
                }
                Ok(resolved_path.trim().to_string())
            }
            Err(e) if !is_from_sandbox => {
                debug!(
                    ?e,
                    "Failed to execute getfattr, but path doesn't seem from a sandbox anyway"
                );
                Ok(path.trim().to_string())
            }
            Err(e) => {
                debug!(?e, "Failed to resolve host path using getfattr");
                Err(e)
            }
        }
    }
}

impl Default for RootStore {
    fn default() -> Self {
        glib::Object::builder().build()
    }
}

#[cfg(test)]
mod tests {
    use std::future::pending;
    use std::io;
    use std::time::Duration;

    use super::*;
    use crate::fakers::NullCommandRunnerBuilder;
    use crate::gtk_utils::test_utils::spin_main_context_until;

    #[gtk::test]
    fn test_resolve_path() {
        let tests = [
            (
                "/run/user/1000/doc/abc123",
                Ok("/home/user/Documents/custom-home-folder"),
                Ok("/home/user/Documents/custom-home-folder"),
            ),
            ("/home/user/Documents/custom-home-folder", Ok(""), {
                Ok("/home/user/Documents/custom-home-folder")
            }),
            ("/run/user/1000/doc/xyz456", Err(()), Err(())),
        ];

        for (input_path, getfattr_output, expected_resolved_path) in tests {
            let runner = NullCommandRunnerBuilder::new()
                .cmd_full(RootStore::getfattr_cmd(input_path), move || {
                    getfattr_output
                        .map(|s| s.to_string())
                        .map_err(|_| io::Error::new(io::ErrorKind::NotFound, "Command not found"))
                })
                .build();
            let store = RootStore::new(runner, Settings::new_null(), FileSystem::new_null());

            let resolved_path: Result<String, backends::Error> =
                smol::block_on(store.resolve_host_path(input_path));

            if let Ok(expected_resolved_path) = expected_resolved_path {
                assert_eq!(resolved_path.unwrap(), expected_resolved_path);
            } else {
                assert!(resolved_path.is_err());
            }
        }
    }

    #[gtk::test]
    fn test_selected_terminal_setting_is_empty() {
        let store = RootStore::new(
            NullCommandRunnerBuilder::new().build(),
            Settings::new_null(),
            FileSystem::new_null(),
        );

        store
            .settings()
            .set_string("selected-terminal", "")
            .expect("failed to set selected-terminal setting");
        assert!(store.selected_terminal_setting_is_empty());

        store
            .settings()
            .set_string("selected-terminal", "GNOME Console")
            .expect("failed to set selected-terminal setting");
        assert!(!store.selected_terminal_setting_is_empty());
    }

    #[gtk::test]
    fn test_ensure_selected_terminal_does_not_overwrite_non_empty_invalid_setting() {
        let runner = NullCommandRunnerBuilder::new()
            .cmd(
                &[
                    "gsettings",
                    "get",
                    "org.gnome.desktop.default-applications.terminal",
                    "exec",
                ],
                "'konsole'\n",
            )
            .build();
        let store = RootStore::new(runner, Settings::new_null(), FileSystem::new_null());

        store
            .settings()
            .set_string("selected-terminal", "Definitely Not A Real Terminal")
            .expect("failed to set selected-terminal setting");

        store.ensure_selected_terminal_after_load();

        spin_main_context_until(Duration::from_millis(200), || {
            !store
                .terminal_repository()
                .json_terminals_query()
                .is_loading()
                && !store
                    .terminal_repository()
                    .flatpak_terminals_query()
                    .is_loading()
        });

        let selected_terminal: String = store.settings().string("selected-terminal");
        assert_eq!(selected_terminal, "Definitely Not A Real Terminal");
    }

    #[gtk::test]
    fn test_ensure_selected_terminal_writes_fallback_when_setting_empty_and_sources_settled() {
        let runner = NullCommandRunnerBuilder::new()
            .cmd(
                &[
                    "gsettings",
                    "get",
                    "org.gnome.desktop.default-applications.terminal",
                    "exec",
                ],
                "'konsole'\n",
            )
            .build();
        let store = RootStore::new(runner, Settings::new_null(), FileSystem::new_null());

        store
            .settings()
            .set_string("selected-terminal", "")
            .expect("failed to set selected-terminal setting");

        store.ensure_selected_terminal_after_load();

        spin_main_context_until(Duration::from_millis(300), || {
            let selected: String = store.settings().string("selected-terminal");
            selected == "Konsole"
        });

        let selected_terminal: String = store.settings().string("selected-terminal");
        assert_eq!(selected_terminal, "Konsole");
    }

    #[gtk::test]
    fn test_ensure_selected_terminal_skips_fallback_while_terminal_sources_loading() {
        let runner = NullCommandRunnerBuilder::new()
            .cmd(
                &[
                    "gsettings",
                    "get",
                    "org.gnome.desktop.default-applications.terminal",
                    "exec",
                ],
                "'konsole'\n",
            )
            .build();
        let store = RootStore::new(runner, Settings::new_null(), FileSystem::new_null());

        store
            .settings()
            .set_string("selected-terminal", "")
            .expect("failed to set selected-terminal setting");

        store
            .terminal_repository()
            .json_terminals_query()
            .set_fetcher(|| async { pending::<anyhow::Result<Vec<Terminal>>>().await });
        store.terminal_repository().json_terminals_query().refetch();

        assert!(
            store
                .terminal_repository()
                .json_terminals_query()
                .is_loading()
        );

        store.ensure_selected_terminal_after_load();

        spin_main_context_until(Duration::from_millis(150), || false);

        let selected_terminal: String = store.settings().string("selected-terminal");
        assert!(selected_terminal.is_empty());
    }

    #[gtk::test]
    fn test_connect_error_completion_does_not_overwrite_non_empty_selected_terminal() {
        let runner = NullCommandRunnerBuilder::new()
            .cmd(
                &[
                    "gsettings",
                    "get",
                    "org.gnome.desktop.default-applications.terminal",
                    "exec",
                ],
                "'konsole'\n",
            )
            .build();
        let store = RootStore::new(runner, Settings::new_null(), FileSystem::new_null());

        store
            .settings()
            .set_string("selected-terminal", "Definitely Not A Real Terminal")
            .expect("failed to set selected-terminal setting");

        store
            .terminal_repository()
            .json_terminals_query()
            .set_fetcher(|| async { Ok(vec![]) });
        store
            .terminal_repository()
            .flatpak_terminals_query()
            .set_fetcher(|| async {
                glib::timeout_future(Duration::from_millis(40)).await;
                Err(anyhow::anyhow!("flatpak terminals query failed"))
            });

        store.terminal_repository().load_all();

        spin_main_context_until(Duration::from_millis(250), || {
            !store
                .terminal_repository()
                .json_terminals_query()
                .is_loading()
                && !store
                    .terminal_repository()
                    .flatpak_terminals_query()
                    .is_loading()
        });

        let selected_terminal: String = store.settings().string("selected-terminal");
        assert_eq!(selected_terminal, "Definitely Not A Real Terminal");
    }

    #[gtk::test]
    fn test_connect_error_completion_writes_fallback_when_selected_terminal_empty() {
        let runner = NullCommandRunnerBuilder::new()
            .cmd(
                &[
                    "gsettings",
                    "get",
                    "org.gnome.desktop.default-applications.terminal",
                    "exec",
                ],
                "'konsole'\n",
            )
            .build();
        let store = RootStore::new(runner, Settings::new_null(), FileSystem::new_null());

        store
            .settings()
            .set_string("selected-terminal", "")
            .expect("failed to set selected-terminal setting");

        store
            .terminal_repository()
            .json_terminals_query()
            .set_fetcher(|| async { Ok(vec![]) });
        store
            .terminal_repository()
            .flatpak_terminals_query()
            .set_fetcher(|| async {
                glib::timeout_future(Duration::from_millis(40)).await;
                Err(anyhow::anyhow!("flatpak terminals query failed"))
            });

        store.terminal_repository().load_all();

        spin_main_context_until(Duration::from_millis(350), || {
            let selected: String = store.settings().string("selected-terminal");
            selected == "Konsole"
        });

        let selected_terminal: String = store.settings().string("selected-terminal");
        assert_eq!(selected_terminal, "Konsole");
    }

    #[gtk::test]
    fn test_selected_terminal_resolution_is_missing_while_sources_loading() {
        let store = RootStore::new(
            NullCommandRunnerBuilder::new().build(),
            Settings::new_null(),
            FileSystem::new_null(),
        );

        store
            .settings()
            .set_string("selected-terminal", "Definitely Not A Real Terminal")
            .expect("failed to set selected-terminal setting");

        store
            .terminal_repository()
            .json_terminals_query()
            .set_fetcher(|| async { pending::<anyhow::Result<Vec<Terminal>>>().await });
        store.terminal_repository().json_terminals_query().refetch();

        assert!(store.terminal_sources_loading());
        assert!(matches!(
            store.selected_terminal_resolution(),
            SelectedTerminalResolution::Missing(name) if name == "Definitely Not A Real Terminal"
        ));
    }

    #[gtk::test]
    fn test_selected_terminal_resolution_supports_legacy_program_setting() {
        let store = RootStore::new(
            NullCommandRunnerBuilder::new().build(),
            Settings::new_null(),
            FileSystem::new_null(),
        );

        store
            .settings()
            .set_string("selected-terminal", "konsole")
            .expect("failed to set selected-terminal setting");

        match store.selected_terminal_resolution() {
            SelectedTerminalResolution::Found(terminal) => {
                assert_eq!(terminal.name, "Konsole");
            }
            _ => panic!("expected selected terminal to resolve by legacy program name"),
        }
    }

    #[gtk::test]
    fn test_download_distrobox_returns_existing_active_task() {
        let store = RootStore::new(
            NullCommandRunnerBuilder::new().build(),
            Settings::new_null(),
            FileSystem::new_null(),
        );
        let existing_task = DistroboxTask::new("system", "Downloading Distrobox", |_task| async {
            pending::<anyhow::Result<()>>().await
        });
        store.tasks().append(&existing_task);

        let returned_task = store.download_distrobox();

        assert_eq!(returned_task, existing_task);
        assert_eq!(store.tasks().iter().count(), 1);
    }

    #[gtk::test]
    fn test_shortcuts_toggle_is_idempotent() {
        let store = RootStore::new(
            NullCommandRunnerBuilder::new().build(),
            Settings::new_null(),
            FileSystem::new_null(),
        );

        assert_eq!(
            store.shortcuts_model().n_items(),
            SHORTCUT_DEFINITIONS.len() as u32
        );

        store.enable_shortcuts();
        assert_eq!(
            store.shortcuts_model().n_items(),
            SHORTCUT_DEFINITIONS.len() as u32
        );

        store.disable_shortcuts();
        assert_eq!(store.shortcuts_model().n_items(), 0);

        store.disable_shortcuts();
        assert_eq!(store.shortcuts_model().n_items(), 0);

        store.enable_shortcuts();
        assert_eq!(
            store.shortcuts_model().n_items(),
            SHORTCUT_DEFINITIONS.len() as u32
        );
    }

    #[gtk::test]
    fn test_host_distrobox_version_detects_no_version_with_null_runner() {
        let store = RootStore::new(
            NullCommandRunnerBuilder::new().build(),
            Settings::new_null(),
            FileSystem::new_null(),
        );

        store.host_distrobox_version().refetch();

        spin_main_context_until(Duration::from_secs(5), || {
            store.host_distrobox_version().is_success()
        });

        assert!(
            store.host_distrobox_version().is_success(),
            "host_distrobox_version should succeed"
        );
        assert!(
            store
                .host_distrobox_version()
                .data()
                .is_some_and(|d| d.is_none()),
            "host_distrobox_version data should be None when no host distrobox is available"
        );
    }

    #[gtk::test]
    fn test_distrobox_version_connect_success_fires_with_null_runner() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;
        use std::sync::atomic::Ordering;

        let store = RootStore::new(
            NullCommandRunnerBuilder::new().build(),
            Settings::new_null(),
            FileSystem::new_null(),
        );

        store
            .settings()
            .set_string("distrobox-executable", "host")
            .expect("distrobox-executable key must exist in schema");

        let fired = Arc::new(AtomicBool::new(false));
        let fired_clone = fired.clone();

        store.distrobox_version().connect_success(move |_exe| {
            fired_clone.store(true, Ordering::SeqCst);
        });

        store.host_distrobox_version().refetch();
        store.distrobox_version().refetch();

        spin_main_context_until(Duration::from_secs(5), || fired.load(Ordering::SeqCst));

        assert!(
            fired.load(Ordering::SeqCst),
            "distrobox_version connect_success should fire"
        );
    }

    #[gtk::test]
    fn test_welcome_view_not_shown_when_host_distrobox_available() {
        use crate::fakers::Command;

        let store = RootStore::new(
            NullCommandRunnerBuilder::new()
                .cmd_full(Command::new_with_args("distrobox", ["version"]), || {
                    Ok("distrobox: 1.8.2.3".to_string())
                })
                .cmd_full(
                    Command::new_with_args("sh", ["-c", "command -v distrobox"]),
                    || Ok("/usr/bin/distrobox".to_string()),
                )
                .build(),
            Settings::new_null(),
            FileSystem::new_null(),
        );

        store
            .settings()
            .set_string("distrobox-executable", "host")
            .expect("distrobox-executable key must exist in schema");

        assert_eq!(
            store.current_view(),
            ViewType::Main,
            "initial view before tasks"
        );

        store.start_background_tasks();

        spin_main_context_until(Duration::from_secs(5), || {
            store.distrobox_version().is_success()
        });

        assert!(
            store.distrobox_version().is_success(),
            "distrobox_version should succeed"
        );
        assert_eq!(
            store.current_view(),
            ViewType::Main,
            "should stay on Main when host distrobox is available"
        );
    }

    #[gtk::test]
    fn test_welcome_view_shown_when_host_distrobox_not_available() {
        let store = RootStore::new(
            NullCommandRunnerBuilder::new().build(),
            Settings::new_null(),
            FileSystem::new_null(),
        );

        store
            .settings()
            .set_string("distrobox-executable", "host")
            .expect("distrobox-executable key must exist in schema");

        assert_eq!(
            store.current_view(),
            ViewType::Main,
            "initial view before tasks"
        );

        store.start_background_tasks();

        spin_main_context_until(Duration::from_secs(5), || {
            store.distrobox_version().is_success()
        });

        assert!(
            store.distrobox_version().is_success(),
            "distrobox_version should succeed even when not available"
        );
        assert!(
            store
                .distrobox_version()
                .data()
                .is_some_and(|d| d.is_none()),
            "distrobox_version should be None when host is not available"
        );
        assert_eq!(
            store.current_view(),
            ViewType::Welcome,
            "should redirect to Welcome when host distrobox is not available"
        );
    }

    #[gtk::test]
    fn test_welcome_view_shown_when_bundled_not_installed() {
        use std::os::unix::process::ExitStatusExt;

        let bundled_path = crate::distrobox_downloader::get_bundled_distrobox_path();
        let mut test_cmd = Command::new("test");
        test_cmd.arg("-e").arg(&bundled_path);

        let store = RootStore::new(
            NullCommandRunnerBuilder::new()
                .cmd_full_with_status(test_cmd, ExitStatusExt::from_raw(1), || Ok(String::new()))
                .build(),
            Settings::new_null(),
            FileSystem::new_null(),
        );

        store
            .settings()
            .set_string("distrobox-executable", "bundled")
            .expect("distrobox-executable key must exist in schema");

        store.start_background_tasks();

        spin_main_context_until(Duration::from_secs(5), || {
            store.distrobox_version().is_success()
        });

        assert!(
            store.distrobox_version().is_success(),
            "distrobox_version should succeed for bundled source"
        );
        assert!(
            store
                .distrobox_version()
                .data()
                .is_some_and(|d| d.is_none()),
            "distrobox_version should be None when bundled is not installed"
        );
        assert_eq!(
            store.current_view(),
            ViewType::Welcome,
            "should redirect to Welcome when bundled is selected but not installed"
        );
    }

    #[gtk::test]
    fn test_no_welcome_redirect_when_bundled_is_installed() {
        use crate::fakers::NullFileSystemBuilder;

        let bundled_path = crate::distrobox_downloader::get_bundled_distrobox_path();
        let mut version_cmd = Command::new(&bundled_path);
        version_cmd.arg("version");

        let file_system = NullFileSystemBuilder::new()
            .file(&bundled_path, "fake distrobox script")
            .build();

        let store = RootStore::new(
            NullCommandRunnerBuilder::new()
                .cmd_full(version_cmd, || Ok("distrobox: 1.8.2.5".to_string()))
                .build(),
            Settings::new_null(),
            file_system,
        );

        store
            .settings()
            .set_string("distrobox-executable", "bundled")
            .expect("distrobox-executable key must exist in schema");

        store.start_background_tasks();

        spin_main_context_until(Duration::from_secs(5), || {
            store.distrobox_version().is_success()
        });

        assert!(
            store.distrobox_version().is_success(),
            "distrobox_version should succeed"
        );
        assert!(
            store
                .distrobox_version()
                .data()
                .is_some_and(|d| d.is_some()),
            "distrobox_version should have data when bundled is installed"
        );
        assert_eq!(
            store.current_view(),
            ViewType::Main,
            "should not redirect to Welcome when bundled is installed"
        );
    }

    #[gtk::test]
    fn test_null_no_version_both_queries_are_none() {
        use std::os::unix::process::ExitStatusExt;

        let runner = NullCommandRunnerBuilder::new()
            .cmd_full_with_status(
                {
                    let mut cmd = Command::new("distrobox");
                    cmd.arg("version");
                    cmd
                },
                ExitStatusExt::from_raw(1),
                || Ok(String::new()),
            )
            .fallback(ExitStatusExt::from_raw(1))
            .build();

        let store = RootStore::new(runner, Settings::new_null(), FileSystem::new_null());

        store.start_background_tasks();

        spin_main_context_until(Duration::from_secs(5), || {
            store.host_distrobox_version().is_success()
                && store.bundled_distrobox_version().is_success()
        });

        assert!(
            store.host_distrobox_version().is_success(),
            "host_distrobox_version should succeed"
        );
        assert!(
            store
                .host_distrobox_version()
                .data()
                .is_some_and(|d| d.is_none()),
            "host_distrobox_version should be None"
        );

        assert!(
            store.bundled_distrobox_version().is_success(),
            "bundled_distrobox_version should succeed"
        );
        assert!(
            store
                .bundled_distrobox_version()
                .data()
                .is_some_and(|d| d.is_none()),
            "bundled_distrobox_version should be None"
        );
    }

    #[gtk::test]
    fn test_initial_view_is_main_with_main_store() {
        let store = RootStore::new(
            NullCommandRunnerBuilder::new().build(),
            Settings::new_null(),
            FileSystem::new_null(),
        );

        assert_eq!(store.current_view(), ViewType::Main);
        assert!(
            store.main_store().is_some(),
            "main_store should exist on Main view"
        );
    }

    #[gtk::test]
    fn test_set_current_view_welcome_swaps_stores() {
        let store = RootStore::new(
            NullCommandRunnerBuilder::new().build(),
            Settings::new_null(),
            FileSystem::new_null(),
        );

        store.set_current_view(ViewType::Welcome);
        assert_eq!(store.current_view(), ViewType::Welcome);
        assert!(
            store.main_store().is_none(),
            "main_store should be dropped when switching to Welcome"
        );

        store.set_current_view(ViewType::Main);
        assert_eq!(store.current_view(), ViewType::Main);
        assert!(
            store.main_store().is_some(),
            "main_store should be recreated when switching back to Main"
        );
    }

    #[gtk::test]
    fn test_set_current_view_fires_notifications() {
        use std::cell::Cell;
        use std::rc::Rc;

        let store = RootStore::new(
            NullCommandRunnerBuilder::new().build(),
            Settings::new_null(),
            FileSystem::new_null(),
        );

        let view_count = Rc::new(Cell::new(0u32));
        let main_count = Rc::new(Cell::new(0u32));

        store.connect_notify_local(Some("current-view"), {
            let view_count = view_count.clone();
            move |_, _| view_count.set(view_count.get() + 1)
        });
        store.connect_notify_local(Some("main-store"), {
            let main_count = main_count.clone();
            move |_, _| main_count.set(main_count.get() + 1)
        });

        store.set_current_view(ViewType::Welcome);

        assert_eq!(view_count.get(), 1, "notify::current-view should fire once");
        assert_eq!(main_count.get(), 1, "notify::main-store should fire once");
    }

    #[gtk::test]
    fn test_set_current_view_same_value_is_noop() {
        use std::cell::Cell;
        use std::rc::Rc;

        let store = RootStore::new(
            NullCommandRunnerBuilder::new().build(),
            Settings::new_null(),
            FileSystem::new_null(),
        );
        let main_before = store.main_store().expect("main_store should exist");

        let view_count = Rc::new(Cell::new(0u32));
        store.connect_notify_local(Some("current-view"), {
            let view_count = view_count.clone();
            move |_, _| view_count.set(view_count.get() + 1)
        });
        let store_count = Rc::new(Cell::new(0u32));
        store.connect_notify_local(Some("main-store"), {
            let store_count = store_count.clone();
            move |_, _| store_count.set(store_count.get() + 1)
        });

        store.set_current_view(ViewType::Main);

        assert_eq!(
            view_count.get(),
            1,
            "GObject auto-notifies current-view on every set (no EXPLICIT_NOTIFY), even for no-op sets"
        );
        assert_eq!(
            store_count.get(),
            0,
            "store properties should not notify when the view does not change"
        );
        let main_after = store.main_store().expect("main_store should still exist");
        assert_eq!(
            main_before, main_after,
            "main_store instance should be unchanged when view does not change"
        );
    }
}
