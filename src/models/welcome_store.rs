use glib::Properties;
use glib::subclass::prelude::*;
use gtk::glib;
use std::cell::OnceCell;

use crate::models::DistroboxExecutable;
use crate::models::RootStore;
use crate::query::Query;

mod imp {
    use super::*;

    #[derive(Properties)]
    #[properties(wrapper_type = super::WelcomeStore)]
    pub struct WelcomeStore {
        pub root_store: OnceCell<RootStore>,
    }

    impl Default for WelcomeStore {
        fn default() -> Self {
            Self {
                root_store: OnceCell::new(),
            }
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for WelcomeStore {}

    #[glib::object_subclass]
    impl ObjectSubclass for WelcomeStore {
        const NAME: &'static str = "WelcomeStore";
        type Type = super::WelcomeStore;
    }
}

glib::wrapper! {
    pub struct WelcomeStore(ObjectSubclass<imp::WelcomeStore>);
}

impl WelcomeStore {
    pub fn new(root_store: &RootStore) -> Self {
        let this: Self = glib::Object::builder().build();
        this.imp()
            .root_store
            .set(root_store.clone())
            .map_err(|_| "root_store already set")
            .unwrap();
        this
    }

    pub fn root_store(&self) -> &RootStore {
        self.imp().root_store.get().unwrap()
    }

    pub fn distrobox_version(&self) -> Query<Option<DistroboxExecutable>> {
        self.root_store().distrobox_version()
    }

    pub fn container_runtime(&self) -> Query<crate::backends::container_runtime::DetectedRuntime> {
        self.root_store().container_runtime()
    }
}

impl Default for WelcomeStore {
    fn default() -> Self {
        glib::Object::builder().build()
    }
}
