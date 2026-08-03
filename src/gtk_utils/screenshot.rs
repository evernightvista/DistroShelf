//! Screenshot capture utility for tests and debugging.
//!
//! Renders any realized widget — or lazily presents a hidden window first —
//! into a PNG file on disk. `save_screenshot_if_requested` is the
//! env-gated convenience used by e2e tests: with `DISTROSHELF_SCREENSHOTS_DIR`
//! unset the calls are no-ops, with it set each call writes
//! `{dir}/{name}.png`, one per asserted UI state.
//!
//! # Render pipeline
//!
//! The widget subtree is snapshotted with `gtk_widget_snapshot_child`, which
//! builds the widget's full render node (CSS background, borders, children)
//! on demand — this is the same node GTK itself renders on screen. GTK
//! removed the public `gtk_widget_snapshot()` API, so it is not available in
//! the pinned gtk4 0.11.1 bindings; `snapshot_child` from the nearest parent
//! reproduces its behavior exactly, and toplevels (which have no parent)
//! are handled by emulating their own snapshot vfunc: each direct child is
//! snapshotted in place. As a result, captures of a toplevel exclude the
//! toplevel's own CSS background (e.g. the window background color).
//!
//! The resulting node is rendered with the `gsk::Renderer` of the widget's
//! surface via `render_texture`, with the widget's logical bounds (its
//! allocation within its parent) as the viewport. The pixel density is
//! whatever the renderer produces (1x on 1x displays, denser on HiDPI) — no
//! forced normalization; the in-module round-trip test documents the actual
//! scale behavior on the developer's display.
//!
//! # File system access
//!
//! The only file operations are writing the PNG itself (`gdk::Texture::save_to_png`)
//! and creating/canonicalizing the destination directory. These go through
//! [`FileSystem::Real`] (the faker's real variant): this utility is test
//! instrumentation that must write real files, and the `Null` faker variant
//! could never serve it, so it is not threaded through the store graph.
//!
//! # Caveats
//!
//! Captures reflect the developer's own theme, fonts, and scale factor —
//! this is an eyeball/debugging tool, not a CI artifact. When screenshots
//! are enabled, windows flash briefly at each capture point; when disabled,
//! tests run exactly as before.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;

use crate::fakers::FileSystem;
use crate::gtk_utils::test_utils::spin_main_context_until;

/// How long to wait for a lazily presented window to become mapped and
/// allocated before giving up.
const PRESENT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum ScreenshotError {
    #[error("widget is not attached to any surface, or was never allocated within the present timeout")]
    NotRealized,
    #[error("widget snapshot produced no render node")]
    EmptySnapshot,
    #[error("no renderer is available for the widget's surface")]
    NoRenderer,
    #[error("rendering the widget subtree produced an empty texture")]
    RenderFailed,
    #[error("saving the PNG failed: {0}")]
    SaveFailed(#[from] glib::BoolError),
    #[error("file system operation failed: {0}")]
    Io(#[from] std::io::Error),
}

/// Renders `widget` into a PNG at `path` and returns the canonicalized path.
///
/// If `widget` is a [`gtk::Window`] that is not visible, it is lazily
/// presented, the thread-default main context is spun until it is mapped
/// and one frame has been rendered (so every descendant has a current
/// allocation), and its prior visibility is restored afterwards. Any other
/// widget must already be realized: its renderer surface is found by walking
/// up `parent()` ancestors to the nearest [`gtk::Native`] (this also covers
/// presented `AdwDialog`s, which libadwaita hosts inside an internal
/// `GtkWindow`). The viewport is the widget's logical bounds within its
/// parent; the pixel density is whatever the renderer produces.
pub fn save_screenshot(
    widget: &impl IsA<gtk::Widget>,
    path: impl AsRef<Path>,
) -> Result<PathBuf, ScreenshotError> {
    let widget = widget.upcast_ref::<gtk::Widget>();

    let hidden_window = if let Some(window) = widget.downcast_ref::<gtk::Window>()
        && !window.is_visible()
    {
        let window = window.clone();
        window.present();

        // Wait for the window to be mapped, then for one rendered frame:
        // descendants only get a current allocation during the first
        // frame's layout phase (e.g. `AdwCarousel` would otherwise be
        // snapped without an allocation and silently dropped from the
        // capture).
        spin_main_context_until(PRESENT_TIMEOUT, || {
            window.is_mapped() && window.frame_clock().is_some()
        });
        let painted = Rc::new(Cell::new(false));
        if let Some(clock) = window.frame_clock() {
            let flag = painted.clone();
            clock.connect_paint(move |_| flag.set(true));
        }
        spin_main_context_until(PRESENT_TIMEOUT, || painted.get());

        Some(window)
    } else {
        None
    };

    let result = capture(widget, path.as_ref());

    if let Some(window) = hidden_window {
        window.set_visible(false);
    }

    result
}

/// Writes `{name}.png` into `$DISTROSHELF_SCREENSHOTS_DIR` if that variable
/// is set, returning the canonicalized path; returns `None` without touching
/// the widget when it is unset. The directory is created recursively and
/// path-unfriendly characters are sanitized out of `name`. Errors propagate
/// to the caller, so opted-in capture failures fail the test loudly.
pub fn save_screenshot_if_requested(
    widget: &impl IsA<gtk::Widget>,
    name: &str,
) -> Result<Option<PathBuf>, ScreenshotError> {
    let Some(dir) = std::env::var_os("DISTROSHELF_SCREENSHOTS_DIR") else {
        return Ok(None);
    };

    let fs = FileSystem::Real;
    let dir = PathBuf::from(dir);
    fs.create_dir_all(&dir)?;

    let path = dir.join(format!("{}.png", sanitize_name(name)));
    let path = save_screenshot(widget, path)?;
    Ok(Some(path))
}

fn capture(widget: &gtk::Widget, path: &Path) -> Result<PathBuf, ScreenshotError> {
    if !widget.is_mapped() {
        return Err(ScreenshotError::NotRealized);
    }

    let native = widget.native().ok_or(ScreenshotError::NotRealized)?;
    let surface = native.surface().ok_or(ScreenshotError::NotRealized)?;
    let renderer = gsk::Renderer::for_surface(&surface).ok_or(ScreenshotError::NoRenderer)?;

    let snapshot = gtk::Snapshot::new();
    snapshot_widget(widget, &snapshot);
    let node = snapshot.to_node().ok_or(ScreenshotError::EmptySnapshot)?;

    // The snapshot is recorded in the coordinate space of the widget's
    // parent (for toplevels, of the toplevel itself), so the viewport is
    // the widget's bounds in that same space.
    let target = widget.parent().unwrap_or_else(|| widget.clone());
    let Some(viewport) = widget.compute_bounds(&target) else {
        return Err(ScreenshotError::NotRealized);
    };
    if viewport.width() <= 0.0 || viewport.height() <= 0.0 {
        return Err(ScreenshotError::NotRealized);
    }
    let texture = renderer.render_texture(&node, Some(&viewport));
    renderer.unrealize();

    if texture.width() == 0 || texture.height() == 0 {
        return Err(ScreenshotError::RenderFailed);
    }

    texture.save_to_png(path)?;
    Ok(FileSystem::Real.canonicalize(path)?)
}

/// Populates `snapshot` with the full render node of `widget`'s subtree.
///
/// Widgets with a parent are snapshotted via `gtk_widget_snapshot_child`,
/// which produces the same render node (CSS background, border, children)
/// that GTK draws on screen, translated to the widget's position in its
/// parent. Toplevels have no parent, so their own snapshot vfunc is
/// emulated: each direct child is snapshotted in place.
fn snapshot_widget(widget: &gtk::Widget, snapshot: &gtk::Snapshot) {
    if let Some(parent) = widget.parent() {
        parent.snapshot_child(widget, snapshot);
        return;
    }

    let mut child = widget.first_child();
    while let Some(c) = child {
        widget.snapshot_child(&c, snapshot);
        child = c.next_sibling();
    }
}

/// Replaces characters that are not safe in file names with `_`.
fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gtk::test]
    fn renders_window_to_png_and_restores_visibility() {
        let window = gtk::Window::new();
        window.set_default_size(320, 240);
        let label = gtk::Label::new(Some("screenshot round trip"));
        window.set_child(Some(&label));

        // Present the window once to obtain its logical bounds: after a
        // window is hidden again its bounds read as empty, so the reference
        // must be taken while it is mapped.
        window.present();
        spin_main_context_until(PRESENT_TIMEOUT, || window.is_mapped());
        let Some(bounds) = window.compute_bounds(&window) else {
            panic!("window must have been allocated before capture");
        };
        assert!(
            bounds.width() > 0.0 && bounds.height() > 0.0,
            "window must have been allocated before capture"
        );
        window.set_visible(false);

        let dir = std::env::temp_dir().join(format!(
            "distroshelf-screenshot-roundtrip-{}",
            std::process::id()
        ));
        let fs = FileSystem::Real;
        fs.create_dir_all(&dir).unwrap();
        let path = dir.join("round-trip.png");

        let saved = save_screenshot(&window, &path).unwrap();

        assert!(
            saved.is_absolute(),
            "returned path must be canonicalized, got {saved:?}"
        );
        assert!(saved.ends_with("round-trip.png"), "got {saved:?}");

        let texture = gtk::gdk::Texture::from_filename(&path)
            .expect("written file must exist and be a valid PNG");
        assert!(
            texture.width() > 0 && texture.height() > 0,
            "PNG must be non-empty"
        );

        let scale_w = texture.width() as f64 / bounds.width() as f64;
        let scale_h = texture.height() as f64 / bounds.height() as f64;
        assert!(
            (1.0..=2.0).contains(&scale_w),
            "texture width {} is not within [1x, 2x] of the logical width {}",
            texture.width(),
            bounds.width()
        );
        assert!(
            (1.0..=2.0).contains(&scale_h),
            "texture height {} is not within [1x, 2x] of the logical height {}",
            texture.height(),
            bounds.height()
        );

        assert!(
            !window.is_visible(),
            "a lazily presented window must be hidden again"
        );

        fs.remove_dir_all(&dir).unwrap();
    }

    #[gtk::test]
    fn unattached_widget_errors_with_not_realized() {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);

        let err = save_screenshot(&widget, "/tmp/never-written.png").unwrap_err();

        assert!(
            matches!(err, ScreenshotError::NotRealized),
            "expected NotRealized, got {err:?}"
        );
    }

    #[test]
    fn sanitize_name_replaces_path_unfriendly_characters() {
        assert_eq!(sanitize_name("e2e_first_boot_host_containers_main"), "e2e_first_boot_host_containers_main");
        assert_eq!(sanitize_name("a/b:c d"), "a_b_c_d");
        assert_eq!(sanitize_name("../evil"), "___evil");
    }
}
