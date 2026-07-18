// End-to-end tests: boot the real application window on top of the
// predefined null command runners and null settings (simulating the first
// boot of the app) and assert on the text the user actually sees.

use std::time::Duration;

use adw::prelude::*;

use crate::application::DistroboxStoreTy;
use crate::fakers::Settings;
use crate::gtk_utils::extract_widget_text;
use crate::gtk_utils::test_utils::spin_main_context_until;
use crate::models::{RootStore, ViewType};
use crate::widgets::DistroShelfWindow;

/// Boots the app like `DistroShelfApplication::recreate_window` does, but
/// against the predefined null command runner of `store_ty` and first-boot
/// (schema default) null settings.
fn boot_first_start(store_ty: DistroboxStoreTy) -> (DistroShelfWindow, RootStore) {
    let _ = adw::init();

    let runner = store_ty
        .null_command_runner()
        .expect("null store type must provide a command runner");
    let settings = Settings::new_null();
    let root_store = RootStore::new(runner, settings);
    root_store.start_background_tasks();

    let window = DistroShelfWindow::new_unattached(root_store.clone());
    (window, root_store)
}

/// The `GtkStack` switching between the "main" and "welcome" views.
fn main_view_stack(window: &DistroShelfWindow) -> gtk::Stack {
    fn find(widget: &gtk::Widget) -> Option<gtk::Stack> {
        if let Some(stack) = widget.downcast_ref::<gtk::Stack>()
            && stack.child_by_name("welcome").is_some()
        {
            return Some(stack.clone());
        }
        let mut child = widget.first_child();
        while let Some(c) = child {
            if let Some(found) = find(&c) {
                return Some(found);
            }
            child = c.next_sibling();
        }
        None
    }
    find(window.upcast_ref()).expect("main/welcome view stack not found")
}

/// All user-visible strings of the currently shown view.
fn visible_view_strings(window: &DistroShelfWindow) -> Vec<String> {
    let visible = main_view_stack(window)
        .visible_child()
        .expect("view stack has a visible child");
    extract_widget_text(&visible)
        .entries
        .into_iter()
        .flat_map(|entry| entry.strings)
        .map(|string| string.value)
        .collect()
}

fn assert_visible_string(strings: &[String], expected: &str) {
    assert!(
        strings.iter().any(|s| s == expected),
        "expected {expected:?} among the visible strings: {strings:#?}"
    );
}

#[gtk::test]
fn test_first_boot_with_host_distrobox_shows_containers() {
    let (window, root_store) = boot_first_start(DistroboxStoreTy::NullHostWorking);

    // Wait for the containers to be rendered in the sidebar, not just
    // loaded in the model.
    spin_main_context_until(Duration::from_secs(5), || {
        visible_view_strings(&window).iter().any(|s| s == "Ubuntu")
    });

    assert!(
        root_store
            .distrobox_version()
            .data()
            .is_some_and(|exe| exe.is_some()),
        "host distrobox must be detected"
    );
    assert!(
        root_store.container_runtime().data().is_some(),
        "podman must be detected as container runtime"
    );
    assert_eq!(
        root_store.current_view(),
        ViewType::Main,
        "a working host distrobox must boot into the main view"
    );
    assert_eq!(
        main_view_stack(&window).visible_child_name().as_deref(),
        Some("main")
    );

    let strings = visible_view_strings(&window);
    for container_name in ["Ubuntu", "Fedora", "Arch Linux", "Alpine"] {
        assert_visible_string(&strings, container_name);
    }

    window.destroy();
}

#[gtk::test]
fn test_first_boot_without_host_distrobox_shows_welcome_requirements() {
    let (window, root_store) = boot_first_start(DistroboxStoreTy::NullNoVersion);

    spin_main_context_until(Duration::from_secs(5), || {
        root_store.current_view() == ViewType::Welcome
            && visible_view_strings(&window)
                .iter()
                .any(|s| s.starts_with("Not found"))
    });

    assert!(
        root_store
            .distrobox_version()
            .data()
            .is_some_and(|exe| exe.is_none()),
        "no distrobox must be detected on first boot without host distrobox"
    );
    assert_eq!(
        root_store.current_view(),
        ViewType::Welcome,
        "a missing distrobox must boot into the welcome view"
    );
    assert_eq!(
        main_view_stack(&window).visible_child_name().as_deref(),
        Some("welcome")
    );

    let strings = visible_view_strings(&window);
    assert_visible_string(&strings, "System Requirements");
    assert_visible_string(&strings, "Distrobox");
    assert_visible_string(
        &strings,
        "Not found - Install from system or use bundled version",
    );
    assert_visible_string(&strings, "Container Runtime");
    assert_visible_string(&strings, "Not found - Please install Podman or Docker");

    window.destroy();
}
