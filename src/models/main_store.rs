use futures::StreamExt;
use futures::TryFutureExt;
use glib::Properties;
use glib::subclass::prelude::*;
use gtk::glib;
use gtk::glib::clone;
use gtk::gio;
use gtk::prelude::*;
use std::cell::OnceCell;
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;
use tracing::{debug, info, warn};

use crate::backends::Status;
use crate::backends::container_runtime::DetectedRuntime;
use crate::backends::podman::PodmanEvent;
use crate::distrobox_init_migration::{StaleContainer, current_init_path, find_stale_containers};
use crate::fakers::{Command, CommandRunner};
use crate::gtk_utils::{TypedListStore, reconcile_list_by_key};
use crate::models::Container;
use crate::models::ContainerSortKey;
use crate::models::DistroboxExecutable;
use crate::query::Query;

mod imp {
    use super::*;

    #[derive(Properties)]
    #[properties(wrapper_type = super::MainStore)]
    pub struct MainStore {
        pub distrobox: OnceCell<crate::backends::Distrobox>,
        pub command_runner: OnceCell<CommandRunner>,
        pub runtime_query: RefCell<Query<DetectedRuntime>>,
        pub distrobox_version: RefCell<Query<Option<DistroboxExecutable>>>,

        pub images_query: Query<Vec<String>>,
        pub downloaded_images_query: Query<HashSet<String>>,
        pub containers_query: Query<Vec<Container>>,

        pub containers: TypedListStore<Container>,
        pub selected_container_model: OnceCell<gtk::SingleSelection>,
        pub sorted_container_model: OnceCell<gtk::SortListModel>,
        pub containers_sorter: OnceCell<gtk::CustomSorter>,

        #[property(get, set, builder(ContainerSortKey::default()))]
        pub containers_sort_key: RefCell<ContainerSortKey>,

        pub settings: OnceCell<gio::Settings>,

        /// Containers whose baked-in `distrobox-init` path no longer exists
        pub stale_containers: TypedListStore<glib::BoxedAnyObject>,
        /// Guards against concurrent stale-container checks
        pub stale_check_running: std::cell::Cell<bool>,
        pub stale_check_pending: std::cell::Cell<bool>,
    }

    impl Default for MainStore {
        fn default() -> Self {
            Self {
                distrobox: OnceCell::new(),
                command_runner: OnceCell::new(),
                runtime_query: RefCell::new(Query::new("runtime".into(), || async {
                    anyhow::bail!("Container runtime not initialized")
                })),
                distrobox_version: RefCell::new(Query::new("distrobox_version".into(), || async {
                    Ok(None)
                })),
                images_query: Query::new("images".into(), || async { Ok(vec![]) }),
                downloaded_images_query: Query::new("downloaded_images".into(), || async {
                    Ok(HashSet::new())
                }),
                containers_query: Query::new("containers".into(), || async { Ok(vec![]) }),
                containers: TypedListStore::new(),
                selected_container_model: OnceCell::new(),
                sorted_container_model: OnceCell::new(),
                containers_sorter: OnceCell::new(),
                containers_sort_key: RefCell::new(ContainerSortKey::default()),
                settings: OnceCell::new(),
                stale_containers: TypedListStore::new(),
                stale_check_running: std::cell::Cell::new(false),
                stale_check_pending: std::cell::Cell::new(false),
            }
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for MainStore {}

    #[glib::object_subclass]
    impl ObjectSubclass for MainStore {
        const NAME: &'static str = "MainStore";
        type Type = super::MainStore;
    }
}

glib::wrapper! {
    pub struct MainStore(ObjectSubclass<imp::MainStore>);
}

impl MainStore {
    pub fn new(
        command_runner: CommandRunner,
        runtime_query: Query<DetectedRuntime>,
        distrobox_version: Query<Option<DistroboxExecutable>>,
        containers_sort_key: ContainerSortKey,
        settings: gio::Settings,
    ) -> Self {
        let this: Self = glib::Object::builder().build();

        let cmd_factory: crate::backends::distrobox::command::CmdFactory = std::rc::Rc::new({
            let dv = distrobox_version.clone();
            move || {
                if let Some(Some(exe)) = dv.data() {
                    debug_assert!(
                        !exe.path().is_empty(),
                        "resolved distrobox path must not be empty"
                    );
                    return Command::new(exe.path().to_owned());
                }
                tracing::warn!("distrobox_version not ready; falling back to bare 'distrobox'");
                Command::new("distrobox")
            }
        });

        let distrobox = crate::backends::Distrobox::new(command_runner.clone(), cmd_factory);

        this.imp()
            .distrobox
            .set(distrobox)
            .map_err(|_| "distrobox already set")
            .unwrap();
        this.imp()
            .command_runner
            .set(command_runner)
            .map_err(|_| "command_runner already set")
            .unwrap();
        this.imp().runtime_query.replace(runtime_query);
        this.imp().distrobox_version.replace(distrobox_version);

        this.imp()
            .settings
            .set(settings)
            .map_err(|_| "settings already set")
            .unwrap();
        this.set_containers_sort_key(containers_sort_key);

        let main_weak = this.downgrade();
        let sorter = gtk::CustomSorter::new(move |obj1, obj2| {
            let Some(main) = main_weak.upgrade() else {
                return gtk::Ordering::Equal;
            };
            let container1 = obj1.downcast_ref::<Container>().unwrap();
            let container2 = obj2.downcast_ref::<Container>().unwrap();
            let sort_key = *main.imp().containers_sort_key.borrow();
            match sort_key {
                ContainerSortKey::Name => container1.name().cmp(&container2.name()).into(),
                ContainerSortKey::CreationDate => {
                    compare_opt_datetimes(container1.creation_date(), container2.creation_date())
                        .into()
                }
                ContainerSortKey::LastUsedDate => {
                    compare_opt_datetimes(container1.last_used_date(), container2.last_used_date())
                        .into()
                }
            }
        });
        this.imp()
            .containers_sorter
            .set(sorter)
            .expect("containers_sorter already set");

        let sorted = gtk::SortListModel::new(
            Some(this.containers().inner().clone()),
            Some(this.imp().containers_sorter.get().unwrap().clone()),
        );

        this.connect_containers_sort_key_notify(clone!(
            #[strong]
            this,
            move |_obj| {
                this.imp()
                    .containers_sorter
                    .get()
                    .unwrap()
                    .changed(gtk::SorterChange::Different);
            }
        ));

        this.connect_containers_sort_key_notify(move |obj| {
            let _ = obj.settings().set_string("sort-key", obj.containers_sort_key().to_str());
        });

        this.imp()
            .sorted_container_model
            .set(sorted)
            .expect("sorted_container_model already set");

        let selection = gtk::SingleSelection::new(Some(this.sorted_container_model()));
        this.imp()
            .selected_container_model
            .set(selection)
            .expect("selected_container_model already set");

        // Set up images fetcher (no ref cycle: captures only distrobox, not MainStore)
        {
            let distrobox = this.distrobox().clone();
            this.imp().images_query.set_fetcher(move || {
                let distrobox = distrobox.clone();
                async move { distrobox.list_images().map_err(|e| e.into()).await }
            });
        }

        // Set up downloaded images fetcher (no ref cycle: captures only runtime_query)
        {
            let runtime_query = this.runtime_query();
            this.imp().downloaded_images_query.set_fetcher(move || {
                let runtime_query = runtime_query.clone();
                async move {
                    runtime_query
                        .data()
                        .ok_or_else(|| anyhow::anyhow!("No container runtime available"))?
                        .runtime
                        .downloaded_images()
                        .await
                }
            });
        }

        // Set up containers fetcher (no ref cycle: captures cloned data, not MainStore)
        {
            let distrobox = this.distrobox().clone();
            let runtime_query = this.runtime_query();
            let main_weak = this.downgrade();
            let on_containers_changed: Rc<dyn Fn()> = Rc::new({
                let main_weak = main_weak.clone();
                move || {
                    if let Some(main) = main_weak.upgrade() {
                        main.load_containers();
                    }
                }
            });
            this.imp().containers_query.set_fetcher(move || {
                let distrobox = distrobox.clone();
                let runtime_query = runtime_query.clone();
                let on_containers_changed = on_containers_changed.clone();
                async move {
                    let mut containers = distrobox.list().await?;

                    let ids: Vec<&str> = containers.values().map(|c| c.id.as_str()).collect();
                    if let Some(detected) = runtime_query.data() {
                        match detected.runtime.inspect_containers(&ids).await {
                            Ok(inspected) => {
                                for (_name, info) in containers.iter_mut() {
                                    if let Some(inspect_info) = inspected.get(&info.id) {
                                        info.created_at = inspect_info.created_at.clone();
                                        info.last_used_at =
                                            inspect_info.last_used_at().map(|s| s.to_string());
                                    }
                                }
                            }
                            Err(e) => {
                                warn!(
                                    error = %e,
                                    "Failed to inspect containers for date info"
                                );
                            }
                        }
                    }

                    let containers: Vec<_> = containers
                        .into_values()
                        .map(|v| {
                            Container::from_info(
                                distrobox.clone(),
                                on_containers_changed.clone(),
                                runtime_query.clone(),
                                v,
                            )
                        })
                        .collect();
                    Ok(containers)
                }
            });
        }
        this.containers_query()
            .set_refetch_strategy(Query::throttle(Duration::from_secs(1), true));

        {
            let main_weak = this.downgrade();
            this.containers_query().connect_success(move |containers| {
                let Some(main) = main_weak.upgrade() else {
                    return;
                };
                reconcile_list_by_key(
                    main.containers(),
                    &containers[..],
                    |item| item.name(),
                    &[
                        "status-tag",
                        "status-detail",
                        "distro",
                        "image",
                        "creation-date",
                        "last-used-date",
                    ],
                );
                if let Some(sorter) = main.imp().containers_sorter.get() {
                    sorter.changed(gtk::SorterChange::Different);
                }
            });
        }

        // Initial stale-container check
        this.check_stale_containers();

        this
    }

    pub fn distrobox(&self) -> &crate::backends::Distrobox {
        self.imp().distrobox.get().unwrap()
    }

    pub fn command_runner(&self) -> CommandRunner {
        self.imp().command_runner.get().unwrap().clone()
    }

    pub fn settings(&self) -> &gio::Settings {
        self.imp().settings.get().unwrap()
    }

    pub fn runtime_query(&self) -> Query<DetectedRuntime> {
        self.imp().runtime_query.borrow().clone()
    }

    pub fn distrobox_version(&self) -> Query<Option<DistroboxExecutable>> {
        self.imp().distrobox_version.borrow().clone()
    }

    pub fn images_query(&self) -> Query<Vec<String>> {
        self.imp().images_query.clone()
    }

    pub fn downloaded_images_query(&self) -> Query<HashSet<String>> {
        self.imp().downloaded_images_query.clone()
    }

    pub fn containers_query(&self) -> Query<Vec<Container>> {
        self.imp().containers_query.clone()
    }

    pub fn containers(&self) -> &TypedListStore<Container> {
        &self.imp().containers
    }

    pub fn stale_containers(&self) -> &TypedListStore<glib::BoxedAnyObject> {
        &self.imp().stale_containers
    }

    pub fn selected_container_model(&self) -> gtk::SingleSelection {
        self.imp().selected_container_model.get().unwrap().clone()
    }

    pub fn sorted_container_model(&self) -> gtk::SortListModel {
        self.imp().sorted_container_model.get().unwrap().clone()
    }

    pub fn selected_container(&self) -> Option<Container> {
        let model = self.selected_container_model();
        let position = model.selected();
        if position == gtk::INVALID_LIST_POSITION {
            None
        } else {
            model
                .selected_item()
                .and_then(|obj| obj.downcast::<Container>().ok())
        }
    }

    pub fn selected_container_name(&self) -> Option<String> {
        self.selected_container().map(|c| c.name())
    }

    pub fn load_containers(&self) {
        self.containers_query().refetch();
    }

    /// Start listening to podman events and auto-refresh container list
    pub fn start_listening_podman_events(&self) {
        let this_weak = self.downgrade();
        let command_runner = self.command_runner();

        glib::MainContext::ref_thread_default().spawn_local(async move {
            info!("Starting podman events listener");
            let podman = crate::backends::podman::Podman::new(Rc::new(command_runner.clone()));

            let stream = match podman.listen_events() {
                Ok(stream) => stream,
                Err(e) => {
                    warn!("Failed to start podman events listener: {}", e);
                    return;
                }
            };

            stream.for_each(move |line_result| {
                let this_weak = this_weak.clone();
                async move {
                    let Some(this) = this_weak.upgrade() else {
                        return;
                    };
                    match line_result {
                        Ok(line) => {
                            match serde_json::from_str::<PodmanEvent>(&line) {
                                Ok(event) => {
                                    if event.is_container_event() && event.is_distrobox() {
                                        debug!(
                                            "Distrobox container event detected ({}), refreshing container list",
                                            event.status.as_deref().unwrap_or("unknown")
                                        );
                                        this.containers_query()
                                            .refetch_if_stale(Duration::from_secs(1));
                                    }
                                }
                                Err(e) => {
                                    debug!(
                                        "Failed to parse podman event JSON: {} - Line: {}",
                                        e, line
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            tracing::error!("Error reading podman event: {}", e);
                        }
                    }
                }
            })
            .await;

            warn!("Podman events listener stopped");
        });
    }

    /// Inspects every container and records in `stale_containers()` those
    /// whose `distrobox-init` bind-mount no longer resolves (see
    /// docs/distrobox-init-migration.md). Runs automatically whenever
    /// the resolved distrobox executable changes and when the container
    /// runtime becomes available.
    ///
    /// Only one check runs at a time; a concurrent call schedules a
    /// single re-run once the current check finishes.
    pub fn check_stale_containers(&self) {
        let imp = self.imp();
        if imp.stale_check_running.get() {
            imp.stale_check_pending.set(true);
            return;
        }
        imp.stale_check_running.set(true);
        let this = self.clone();
        glib::MainContext::ref_thread_default().spawn_local(async move {
            loop {
                this.run_stale_check().await;
                if !this.imp().stale_check_pending.replace(false) {
                    break;
                }
            }
            this.imp().stale_check_running.set(false);
        });
    }

    async fn run_stale_check(&self) {
        let Some(Some(exe)) = self.distrobox_version().data() else {
            if !self.stale_containers().is_empty() {
                self.stale_containers().remove_all();
            }
            return;
        };
        let Some(current_init) = current_init_path(exe.path()) else {
            warn!(
                path = %exe.path(),
                "Cannot determine distrobox-init location; skipping stale container check"
            );
            return;
        };
        let Some(detected) = self.runtime_query().data() else {
            debug!("Container runtime not available yet; skipping stale container check");
            return;
        };
        let containers = match self.distrobox().list().await {
            Ok(containers) => containers,
            Err(e) => {
                warn!(error = %e, "Failed to list containers for stale-init check");
                return;
            }
        };
        let containers: Vec<(String, bool)> = containers
            .into_values()
            .map(|c| (c.name, matches!(c.status, Status::Up(_))))
            .collect();

        let stale = find_stale_containers(
            &self.command_runner(),
            detected.runtime.as_ref(),
            &containers,
            &current_init,
        )
        .await;

        if !stale.is_empty() {
            info!(
                count = stale.len(),
                "Found containers with stale distrobox-init paths"
            );
        }
        let new_items: Vec<glib::BoxedAnyObject> =
            stale.into_iter().map(glib::BoxedAnyObject::new).collect();
        reconcile_list_by_key(
            self.stale_containers(),
            &new_items[..],
            |obj| obj.borrow::<StaleContainer>().clone(),
            &[],
        );
    }

    pub fn all_containers(&self) -> Vec<Container> {
        self.containers().iter().collect()
    }
}

impl Default for MainStore {
    fn default() -> Self {
        glib::Object::builder().build()
    }
}

fn compare_opt_datetimes(
    a: Option<glib::DateTime>,
    b: Option<glib::DateTime>,
) -> std::cmp::Ordering {
    match (a.map(|d| d.to_unix()), b.map(|d| d.to_unix())) {
        (Some(a), Some(b)) => b.cmp(&a), // descending: newest/most-recent first
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt;
    use std::path::PathBuf;

    use super::*;
    use crate::backends::podman::Podman;
    use crate::backends::{ContainerInfo, Distrobox, DistroboxCommandRunnerResponse};
    use crate::fakers::NullCommandRunnerBuilder;
    use crate::gtk_utils::test_utils::spin_main_context_until;
    use crate::models::VersionedExecutable;

    const STALE_INIT: &str = "/home/user/.local/share/distroshelf/distrobox-1.8.2.1/distrobox-init";

    fn version_query(path: &str) -> Query<Option<DistroboxExecutable>> {
        Query::pure(Some(DistroboxExecutable::Host(VersionedExecutable {
            version: "1.8.2".into(),
            path: path.into(),
        })))
    }

    fn no_runtime_query() -> Query<DetectedRuntime> {
        Query::new("runtime".into(), || async {
            anyhow::bail!("Container runtime not initialized")
        })
    }

    fn test_settings() -> gio::Settings {
        gio::Settings::new("com.ranfdev.DistroShelf")
    }

    fn container_info(
        id: &str,
        name: &str,
        created_at: Option<&str>,
        last_used_at: Option<&str>,
    ) -> ContainerInfo {
        ContainerInfo {
            id: id.into(),
            name: name.into(),
            status: Status::Created("2 minutes ago".into()),
            image: "docker.io/library/ubuntu:latest".into(),
            created_at: created_at.map(|s| s.to_string()),
            last_used_at: last_used_at.map(|s| s.to_string()),
        }
    }

    fn sorted_names(model: &gtk::SortListModel) -> Vec<String> {
        (0..model.n_items())
            .map(|i| {
                model
                    .item(i)
                    .unwrap()
                    .downcast::<Container>()
                    .unwrap()
                    .name()
            })
            .collect()
    }

    #[gtk::test]
    fn test_containers_query_populates_containers() {
        let runner = Distrobox::null_command_runner(&[DistroboxCommandRunnerResponse::List(vec![
            container_info("1", "Ubuntu", None, None),
            container_info("2", "Fedora", None, None),
        ])]);
        let store = MainStore::new(runner, no_runtime_query(), version_query("distrobox"), ContainerSortKey::default(), test_settings());

        store.load_containers();
        spin_main_context_until(Duration::from_secs(5), || {
            store.containers().iter().count() == 2
        });

        assert!(
            store.containers_query().is_success(),
            "containers_query should succeed"
        );
        let names: Vec<String> = store.containers().iter().map(|c| c.name()).collect();
        assert_eq!(names, vec!["Fedora", "Ubuntu"]);
    }

    #[gtk::test]
    fn test_containers_reconcile_preserves_identity_and_updates_status() {
        let output = Rc::new(RefCell::new(String::from(
            "ID | NAME | STATUS | IMAGE\n\
             1 | Ubuntu | Created 2 minutes ago | docker.io/library/ubuntu:latest\n\
             2 | Fedora | Created 2 minutes ago | docker.io/library/fedora:latest\n",
        )));
        let runner = {
            let output = output.clone();
            NullCommandRunnerBuilder::new()
                .cmd_full(
                    Command::new_with_args("distrobox", ["ls", "--no-color"]),
                    move || Ok(output.borrow().clone()),
                )
                .build()
        };
        let store = MainStore::new(runner, no_runtime_query(), version_query("distrobox"), ContainerSortKey::default(), test_settings());

        store.load_containers();
        spin_main_context_until(Duration::from_secs(5), || {
            store.containers().iter().count() == 2
        });

        let ubuntu_before = store
            .containers()
            .iter()
            .find(|c| c.name() == "Ubuntu")
            .expect("Ubuntu should be listed");
        assert_eq!(ubuntu_before.status_tag(), "created");

        *output.borrow_mut() = String::from(
            "ID | NAME | STATUS | IMAGE\n\
             1 | Ubuntu | Up 3 minutes | docker.io/library/ubuntu:latest\n\
             3 | Arch | Created 1 minute ago | docker.io/library/archlinux:latest\n",
        );
        store.containers_query().fetch();
        spin_main_context_until(Duration::from_secs(5), || {
            store.containers().iter().any(|c| c.name() == "Arch")
        });

        let names: HashSet<String> = store.containers().iter().map(|c| c.name()).collect();
        assert_eq!(
            names,
            HashSet::from(["Ubuntu".to_string(), "Arch".to_string()]),
            "Fedora should be removed, Arch added"
        );
        let ubuntu_after = store
            .containers()
            .iter()
            .find(|c| c.name() == "Ubuntu")
            .expect("Ubuntu should still be listed");
        assert_eq!(
            ubuntu_before, ubuntu_after,
            "reconcile should preserve the Container instance"
        );
        assert_eq!(
            ubuntu_after.status_tag(),
            "up",
            "reconcile should update the status of the preserved instance"
        );
    }

    #[gtk::test]
    fn test_sort_key_reorders_sorted_model() {
        let store = MainStore::new(
            NullCommandRunnerBuilder::new().build(),
            no_runtime_query(),
            version_query("distrobox"),
            ContainerSortKey::default(),
            test_settings(),
        );

        let noop: Rc<dyn Fn()> = Rc::new(|| {});
        let make = |id: &str, name: &str, created_at: Option<&str>, last_used_at: Option<&str>| {
            Container::from_info(
                store.distrobox().clone(),
                noop.clone(),
                store.runtime_query(),
                container_info(id, name, created_at, last_used_at),
            )
        };
        store.containers().append(&make(
            "1",
            "beta",
            Some("2024-01-01T00:00:00Z"),
            Some("2023-06-01T00:00:00Z"),
        ));
        store.containers().append(&make(
            "2",
            "alpha",
            Some("2023-01-01T00:00:00Z"),
            Some("2025-02-01T00:00:00Z"),
        ));
        store.containers().append(&make(
            "3",
            "gamma",
            Some("2025-01-01T00:00:00Z"),
            Some("2024-06-01T00:00:00Z"),
        ));
        store.containers().append(&make("4", "delta", None, None));

        let sorted = store.sorted_container_model();
        assert_eq!(
            sorted_names(&sorted),
            ["alpha", "beta", "delta", "gamma"],
            "default sort key should order by name"
        );

        store.set_containers_sort_key(ContainerSortKey::CreationDate);
        assert_eq!(
            sorted_names(&sorted),
            ["gamma", "beta", "alpha", "delta"],
            "creation-date sort should order newest first with missing dates last"
        );

        store.set_containers_sort_key(ContainerSortKey::LastUsedDate);
        assert_eq!(
            sorted_names(&sorted),
            ["alpha", "gamma", "beta", "delta"],
            "last-used-date sort should order newest first with missing dates last"
        );

        store.set_containers_sort_key(ContainerSortKey::Name);
        assert_eq!(sorted_names(&sorted), ["alpha", "beta", "delta", "gamma"]);
    }

    #[gtk::test]
    fn test_selected_container_none_when_empty_then_first_after_load() {
        let runner = Distrobox::null_command_runner(&[DistroboxCommandRunnerResponse::List(vec![
            container_info("1", "Ubuntu", None, None),
            container_info("2", "Fedora", None, None),
        ])]);
        let store = MainStore::new(runner, no_runtime_query(), version_query("distrobox"), ContainerSortKey::default(), test_settings());

        assert!(store.selected_container().is_none());
        assert!(store.selected_container_name().is_none());

        store.load_containers();
        spin_main_context_until(Duration::from_secs(5), || {
            store.selected_container().is_some()
        });

        assert_eq!(
            store.selected_container_name(),
            Some("Fedora".to_string()),
            "SingleSelection should auto-select the first sorted container"
        );
    }

    #[gtk::test]
    fn test_check_stale_containers_flags_stale_container() {
        let mounts_json = format!(
            r#"[{{"Type":"bind","Source":"{}","Destination":"/usr/bin/entrypoint","Mode":"ro","RW":false,"Propagation":"rprivate"}}]"#,
            STALE_INIT
        );
        let runner = NullCommandRunnerBuilder::new()
            .cmd(
                &["/usr/bin/distrobox", "ls", "--no-color"],
                "ID | NAME | STATUS | IMAGE\n\
                 1 | Ubuntu | Created 2 minutes ago | docker.io/library/ubuntu:latest\n",
            )
            .cmd_full(
                Command::new_with_args(
                    "podman",
                    ["inspect", "--format", "{{ json .Mounts }}", "Ubuntu"],
                ),
                move || Ok(mounts_json.clone()),
            )
            .cmd_full_with_status(
                Command::new_with_args("test", ["-e", STALE_INIT]),
                ExitStatusExt::from_raw(1),
                || Ok(String::new()),
            )
            .build();
        let runtime = DetectedRuntime {
            runtime: Rc::new(Podman::new(Rc::new(runner.clone()))),
            version: "4.9.3".into(),
        };
        let store = MainStore::new(
            runner,
            Query::pure(runtime),
            version_query("/usr/bin/distrobox"),
            ContainerSortKey::default(),
            test_settings(),
        );

        store.check_stale_containers();
        spin_main_context_until(Duration::from_secs(5), || {
            !store.stale_containers().is_empty()
        });

        let stale: Vec<StaleContainer> = store
            .stale_containers()
            .iter()
            .map(|obj| obj.borrow::<StaleContainer>().clone())
            .collect();
        assert_eq!(
            stale,
            vec![StaleContainer {
                name: "Ubuntu".to_string(),
                stale_init_path: PathBuf::from(STALE_INIT),
                running: false,
            }]
        );
    }

    #[gtk::test]
    fn test_check_stale_containers_clears_when_no_distrobox_version() {
        let store = MainStore::new(
            NullCommandRunnerBuilder::new().build(),
            no_runtime_query(),
            Query::pure(None),
            ContainerSortKey::default(),
            test_settings(),
        );
        store
            .stale_containers()
            .append(&glib::BoxedAnyObject::new(StaleContainer {
                name: "ghost".to_string(),
                stale_init_path: PathBuf::from(STALE_INIT),
                running: false,
            }));

        store.check_stale_containers();
        spin_main_context_until(Duration::from_secs(5), || {
            store.stale_containers().is_empty()
        });

        assert!(
            store.stale_containers().is_empty(),
            "stale containers should be cleared when no distrobox executable is resolved"
        );
    }

    #[gtk::test]
    fn test_check_stale_containers_coalesces_concurrent_calls() {
        let store = MainStore::new(
            NullCommandRunnerBuilder::new().build(),
            no_runtime_query(),
            Query::pure(None),
            ContainerSortKey::default(),
            test_settings(),
        );

        assert!(
            store.imp().stale_check_running.get(),
            "constructor should have started a stale check"
        );

        store.check_stale_containers();
        assert!(
            store.imp().stale_check_pending.get(),
            "a concurrent call should be recorded as pending, not run in parallel"
        );

        spin_main_context_until(Duration::from_secs(5), || {
            !store.imp().stale_check_running.get()
        });

        assert!(!store.imp().stale_check_running.get());
        assert!(
            !store.imp().stale_check_pending.get(),
            "the pending re-run should have been consumed"
        );
    }
}
