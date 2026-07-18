// Settings abstraction with a real GSettings-backed variant and a Null
// variant with configurable in-memory responses, to ease code testing.
// Follows the same "Nullable" pattern as `CommandRunner`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, glib};

pub const SETTINGS_SCHEMA_ID: &str = "com.ranfdev.DistroShelf";

type ChangedHandler = Rc<dyn Fn(&str)>;
type ChangedHandlers = Rc<RefCell<Vec<(Option<String>, ChangedHandler)>>>;

/// A single settings instance shared across the whole app.
///
/// `Real` reads and writes the GSettings schema `com.ranfdev.DistroShelf`.
/// `Null` keeps values in memory, so tests and UI previews never touch the
/// host configuration.
#[derive(Clone)]
pub enum Settings {
    Real(gio::Settings),
    Null(NullSettings),
}

impl Settings {
    pub fn new_real() -> Self {
        Settings::Real(gio::Settings::new(SETTINGS_SCHEMA_ID))
    }

    /// A null settings instance pre-filled with the schema defaults,
    /// simulating a first boot of the app.
    pub fn new_null() -> Self {
        NullSettingsBuilder::new().build()
    }

    pub fn string(&self, key: &str) -> String {
        match self {
            Settings::Real(settings) => String::from(settings.string(key)),
            Settings::Null(null) => null.get(key),
        }
    }

    pub fn set_string(&self, key: &str, value: &str) -> Result<(), glib::BoolError> {
        match self {
            Settings::Real(settings) => settings.set_string(key, value),
            Settings::Null(null) => {
                null.set(key, value.to_variant());
                Ok(())
            }
        }
    }

    pub fn boolean(&self, key: &str) -> bool {
        match self {
            Settings::Real(settings) => settings.boolean(key),
            Settings::Null(null) => null.get(key),
        }
    }

    pub fn set_boolean(&self, key: &str, value: bool) -> Result<(), glib::BoolError> {
        match self {
            Settings::Real(settings) => settings.set_boolean(key, value),
            Settings::Null(null) => {
                null.set(key, value.to_variant());
                Ok(())
            }
        }
    }

    pub fn int(&self, key: &str) -> i32 {
        match self {
            Settings::Real(settings) => settings.int(key),
            Settings::Null(null) => null.get(key),
        }
    }

    pub fn set_int(&self, key: &str, value: i32) -> Result<(), glib::BoolError> {
        match self {
            Settings::Real(settings) => settings.set_int(key, value),
            Settings::Null(null) => {
                null.set(key, value.to_variant());
                Ok(())
            }
        }
    }

    /// Invokes `f` with the changed key whenever `key` changes
    /// (or any key, when `key` is `None`).
    pub fn connect_changed(&self, key: Option<&str>, f: impl Fn(&str) + 'static) {
        match self {
            Settings::Real(settings) => {
                settings.connect_changed(key, move |_, changed_key| f(changed_key));
            }
            Settings::Null(null) => {
                null.handlers
                    .borrow_mut()
                    .push((key.map(str::to_string), Rc::new(f)));
            }
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Settings::new_null()
    }
}

impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Settings::Real(_) => f.write_str("Settings::Real"),
            Settings::Null(null) => f
                .debug_tuple("Settings::Null")
                .field(&*null.values.borrow())
                .finish(),
        }
    }
}

/// In-memory settings storage. Unknown keys and type mismatches panic,
/// mirroring how GSettings aborts on schema violations.
#[derive(Clone, Default)]
pub struct NullSettings {
    values: Rc<RefCell<HashMap<String, glib::Variant>>>,
    handlers: ChangedHandlers,
}

impl NullSettings {
    fn get<T: glib::variant::FromVariant>(&self, key: &str) -> T {
        let values = self.values.borrow();
        let variant = values
            .get(key)
            .unwrap_or_else(|| panic!("NullSettings: key {key:?} not configured"));
        variant.get::<T>().unwrap_or_else(|| {
            panic!(
                "NullSettings: key {key:?} holds a value of type {:?}",
                variant.type_()
            )
        })
    }

    fn set(&self, key: &str, value: glib::Variant) {
        {
            let mut values = self.values.borrow_mut();
            let existing = values
                .get_mut(key)
                .unwrap_or_else(|| panic!("NullSettings: key {key:?} not configured"));
            assert!(
                existing.type_() == value.type_(),
                "NullSettings: cannot write a {:?} to key {key:?} of type {:?}",
                value.type_(),
                existing.type_()
            );
            *existing = value;
        }
        self.emit_changed(key);
    }

    fn emit_changed(&self, key: &str) {
        // Collect the matching handlers first: a handler may call
        // connect_changed again, which would otherwise re-borrow `handlers`.
        let matching: Vec<ChangedHandler> = self
            .handlers
            .borrow()
            .iter()
            .filter(|(filter, _)| filter.as_deref().is_none_or(|k| k == key))
            .map(|(_, handler)| handler.clone())
            .collect();
        for handler in matching {
            handler(key);
        }
    }
}

/// Builds a [`Settings::Null`] with custom responses. Starts from the schema
/// defaults, so only the keys under test need to be overridden.
///
/// Keep the defaults in sync with `data/com.ranfdev.DistroShelf.gschema.xml`.
#[derive(Clone)]
pub struct NullSettingsBuilder {
    values: HashMap<String, glib::Variant>,
}

impl NullSettingsBuilder {
    pub fn new() -> Self {
        let values = HashMap::from([
            ("selected-terminal".to_string(), "".to_variant()),
            ("window-width".to_string(), 900i32.to_variant()),
            ("window-height".to_string(), 700i32.to_variant()),
            ("distrobox-executable".to_string(), "host".to_variant()),
            ("distrobox-create-no-entry".to_string(), true.to_variant()),
            ("sort-key".to_string(), "name".to_variant()),
        ]);
        Self { values }
    }

    #[allow(dead_code)]
    pub fn string(&mut self, key: &str, value: &str) -> &mut Self {
        self.value(key, value.to_variant())
    }

    #[allow(dead_code)]
    pub fn boolean(&mut self, key: &str, value: bool) -> &mut Self {
        self.value(key, value.to_variant())
    }

    #[allow(dead_code)]
    pub fn int(&mut self, key: &str, value: i32) -> &mut Self {
        self.value(key, value.to_variant())
    }

    pub fn value(&mut self, key: &str, value: glib::Variant) -> &mut Self {
        self.values.insert(key.to_string(), value);
        self
    }

    pub fn build(&self) -> Settings {
        Settings::Null(NullSettings {
            values: Rc::new(RefCell::new(self.values.clone())),
            handlers: Rc::default(),
        })
    }
}

impl Default for NullSettingsBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn test_null_settings_schema_defaults() {
        let settings = Settings::new_null();

        assert_eq!(settings.string("selected-terminal"), "");
        assert_eq!(settings.int("window-width"), 900);
        assert_eq!(settings.int("window-height"), 700);
        assert_eq!(settings.string("distrobox-executable"), "host");
        assert!(settings.boolean("distrobox-create-no-entry"));
        assert_eq!(settings.string("sort-key"), "name");
    }

    #[test]
    fn test_null_settings_custom_responses() {
        let settings = NullSettingsBuilder::new()
            .string("distrobox-executable", "bundled")
            .int("window-width", 1234)
            .boolean("distrobox-create-no-entry", false)
            .build();

        assert_eq!(settings.string("distrobox-executable"), "bundled");
        assert_eq!(settings.int("window-width"), 1234);
        assert!(!settings.boolean("distrobox-create-no-entry"));
        // Untouched keys keep the schema defaults.
        assert_eq!(settings.string("sort-key"), "name");
    }

    #[test]
    fn test_null_settings_set_and_get_roundtrip() {
        let settings = Settings::new_null();

        settings.set_string("selected-terminal", "Konsole").unwrap();
        settings.set_int("window-width", 640).unwrap();
        settings
            .set_boolean("distrobox-create-no-entry", false)
            .unwrap();

        assert_eq!(settings.string("selected-terminal"), "Konsole");
        assert_eq!(settings.int("window-width"), 640);
        assert!(!settings.boolean("distrobox-create-no-entry"));
    }

    #[test]
    fn test_null_settings_connect_changed_with_key_filter() {
        let settings = Settings::new_null();
        let fired = Rc::new(Cell::new(0u32));

        let fired_clone = fired.clone();
        settings.connect_changed(Some("sort-key"), move |key| {
            assert_eq!(key, "sort-key");
            fired_clone.set(fired_clone.get() + 1);
        });

        settings.set_string("selected-terminal", "xterm").unwrap();
        assert_eq!(fired.get(), 0, "non-matching key must not fire");

        settings.set_string("sort-key", "creation-date").unwrap();
        assert_eq!(fired.get(), 1);
    }

    #[test]
    fn test_null_settings_connect_changed_without_filter_fires_for_all_keys() {
        let settings = Settings::new_null();
        let keys = Rc::new(RefCell::new(Vec::new()));

        let keys_clone = keys.clone();
        settings.connect_changed(None, move |key| {
            keys_clone.borrow_mut().push(key.to_string());
        });

        settings.set_string("sort-key", "last-used").unwrap();
        settings.set_int("window-height", 100).unwrap();

        assert_eq!(*keys.borrow(), vec!["sort-key", "window-height"]);
    }

    #[test]
    fn test_null_settings_handler_can_connect_more_handlers() {
        let settings = Settings::new_null();
        let settings_clone = settings.clone();

        settings.connect_changed(None, move |_| {
            settings_clone.connect_changed(None, |_| {});
        });

        settings.set_string("sort-key", "creation-date").unwrap();
    }

    #[test]
    #[should_panic(expected = "not configured")]
    fn test_null_settings_unknown_key_panics() {
        let settings = Settings::new_null();
        settings.string("no-such-key");
    }

    #[test]
    #[should_panic(expected = "cannot write")]
    fn test_null_settings_type_mismatch_panics() {
        let settings = Settings::new_null();
        let _ = settings.set_int("selected-terminal", 42);
    }
}
