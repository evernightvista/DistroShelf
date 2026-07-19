use crate::{
    backends::{
        ContainerInfo, Distrobox, Status, container_runtime::DetectedRuntime,
        container_runtime::Usage,
    },
    gtk_utils::TypedListStore,
    models::{KnownDistro, known_distro_by_image},
    query::Query,
};

use adw::prelude::*;
use glib::subclass::prelude::*;
use gtk::glib;
use gtk::glib::{BoxedAnyObject, Properties, derived_properties};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

mod imp {
    use super::*;

    // This contains all the container informations given by distrobox, plus an associated KnownDistro struct
    #[derive(Properties)]
    #[properties(wrapper_type=super::Container)]
    pub struct Container {
        #[property(get)]
        pub name: RefCell<String>,
        #[property(get, set)]
        pub status_tag: RefCell<String>,
        #[property(get, set)]
        pub status_detail: RefCell<String>,
        #[property(get, set)]
        pub image: RefCell<String>,
        #[property(get, set, nullable)]
        pub distro: RefCell<Option<KnownDistro>>,
        #[property(get, set, nullable)]
        pub creation_date: RefCell<Option<glib::DateTime>>,
        #[property(get, set, nullable)]
        pub last_used_date: RefCell<Option<glib::DateTime>>,
        pub apps: Query<TypedListStore<glib::BoxedAnyObject>>,
        pub binaries: Query<TypedListStore<glib::BoxedAnyObject>>,
        // Usage statistics, without polling
        pub usage: Query<Usage>,
    }

    impl Default for Container {
        fn default() -> Self {
            Self {
                name: RefCell::new(String::new()),
                status_tag: RefCell::new(String::new()),
                status_detail: RefCell::new(String::new()),
                image: RefCell::new(String::new()),
                distro: RefCell::new(None),
                creation_date: RefCell::new(None),
                last_used_date: RefCell::new(None),

                // Fetching apps often fails when the container is not running and distrobox has to start it,
                // so we add retries
                apps: {
                    let q = Query::new("apps".into(), || async { Ok(TypedListStore::new()) });
                    q.set_timeout(Duration::from_secs(10));
                    q.set_retry_strategy(|n| {
                        if n < 3 {
                            Some(Duration::from_secs(n as u64))
                        } else {
                            None
                        }
                    });
                    q
                },
                binaries: {
                    let q = Query::new("binaries".into(), || async { Ok(TypedListStore::new()) });
                    q.set_timeout(Duration::from_secs(10));
                    q.set_retry_strategy(|n| {
                        if n < 3 {
                            Some(Duration::from_secs(n as u64))
                        } else {
                            None
                        }
                    });
                    q
                },
                usage: {
                    Query::new("usage".into(), || async { Ok(Usage::default()) })
                        .with_priority(glib::Priority::LOW)
                },
            }
        }
    }

    #[derived_properties]
    impl ObjectImpl for Container {}

    #[glib::object_subclass]
    impl ObjectSubclass for Container {
        const NAME: &'static str = "Container";
        type Type = super::Container;
    }
}

glib::wrapper! {
    pub struct Container(ObjectSubclass<imp::Container>);
}
impl Container {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }
    pub fn from_info(
        distrobox: Distrobox,
        on_containers_changed: Rc<dyn Fn()>,
        runtime_query: Query<DetectedRuntime>,
        value: ContainerInfo,
    ) -> Self {
        let this: Self = glib::Object::builder().build();

        this.apply_container_info(value);

        let container_name = this.name();
        this.apps()
            .set_resource_key(&format!("{container_name}:apps"));
        this.binaries()
            .set_resource_key(&format!("{container_name}:binaries"));
        this.usage()
            .set_resource_key(&format!("{container_name}:usage"));

        let this_clone = this.clone();
        let apps_distrobox = distrobox.clone();
        let apps_on_containers_changed = on_containers_changed.clone();
        this.apps().set_fetcher(move || {
            let this = this_clone.clone();
            let distrobox = apps_distrobox.clone();
            let on_containers_changed = apps_on_containers_changed.clone();
            async move {
                let apps = distrobox.list_apps(&this.name()).await?;

                let apps_list: TypedListStore<BoxedAnyObject> =
                    TypedListStore::from_iter(apps.into_iter().map(BoxedAnyObject::new));

                // Listing the apps starts the container, we need to update its status
                on_containers_changed();
                Ok(apps_list)
            }
        });

        let this_clone = this.clone();
        let binaries_distrobox = distrobox.clone();
        let binaries_on_containers_changed = on_containers_changed.clone();
        this.binaries().set_fetcher(move || {
            let this = this_clone.clone();
            let distrobox = binaries_distrobox.clone();
            let on_containers_changed = binaries_on_containers_changed.clone();
            async move {
                let binaries = distrobox.get_exported_binaries(&this.name()).await?;

                let binaries_list: TypedListStore<BoxedAnyObject> =
                    TypedListStore::from_iter(binaries.into_iter().map(BoxedAnyObject::new));

                // Listing the binaries starts the container, we need to update its status
                on_containers_changed();
                Ok(binaries_list)
            }
        });

        let this_clone = this.clone();
        let runtime_query = runtime_query.clone();
        let usage_distrobox = distrobox.clone();
        this.usage().set_fetcher(move || {
            let this = this_clone.clone();
            let runtime_query = runtime_query.clone();
            let distrobox = usage_distrobox.clone();
            async move {
                let runtime = runtime_query
                    .data()
                    .ok_or_else(|| anyhow::anyhow!("Container runtime not available"))?
                    .runtime;
                let usage = runtime
                    .usage(distrobox.command_runner(), &this.name())
                    .await?;
                Ok(usage)
            }
        });

        this
    }

    fn apply_container_info(&self, value: ContainerInfo) {
        let distro = known_distro_by_image(&value.image);

        let (status_tag, status_detail) = match value.status {
            Status::Up(v) => ("up", v),
            Status::Created(v) => ("created", v),
            Status::Exited(v) => ("exited", v),
            Status::Other(v) => ("other", v),
        };

        *self.imp().name.borrow_mut() = value.name;
        self.set_image(value.image);
        self.set_distro(distro);
        self.set_status_tag(status_tag.to_string());
        self.set_status_detail(status_detail);
        self.set_creation_date(parse_iso8601(value.created_at.as_deref()));
        self.set_last_used_date(parse_iso8601(value.last_used_at.as_deref()));
    }

    pub fn is_running(&self) -> bool {
        self.status_tag() == "up"
    }

    pub fn apps(&self) -> Query<TypedListStore<BoxedAnyObject>> {
        self.imp().apps.clone()
    }

    pub fn binaries(&self) -> Query<TypedListStore<BoxedAnyObject>> {
        self.imp().binaries.clone()
    }

    pub fn usage(&self) -> Query<Usage> {
        self.imp().usage.clone()
    }
}

impl Default for Container {
    fn default() -> Self {
        Self::new()
    }
}

fn parse_iso8601(s: Option<&str>) -> Option<glib::DateTime> {
    s.and_then(|s| {
        let tz = gtk::glib::TimeZone::utc();
        glib::DateTime::from_iso8601(s, Some(&tz)).ok()
    })
}
