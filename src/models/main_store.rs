use futures::StreamExt;
use futures::TryFutureExt;
use glib::Properties;
use glib::subclass::prelude::*;
use gtk::glib::clone;
use gtk::prelude::*;
use gtk::glib;
use std::cell::OnceCell;
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;
use tracing::{debug, info, warn};

use crate::backends::container_runtime::DetectedRuntime;
use crate::backends::podman::PodmanEvent;
use crate::backends::Status;
use crate::distrobox_init_migration::{
    StaleContainer, current_init_path, find_stale_containers,
};
use crate::fakers::{Command, CommandRunner};
use crate::gtk_utils::{TypedListStore, reconcile_list_by_key};
use crate::models::Container;
use crate::models::DistroboxExecutable;
use crate::models::ContainerSortKey;
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
    ) -> Self {
        let this: Self = glib::Object::builder().build();

        let cmd_factory: crate::backends::distrobox::command::CmdFactory =
            std::rc::Rc::new({
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

        let distrobox =
            crate::backends::Distrobox::new(command_runner.clone(), cmd_factory);

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
