# Criterion: GObject and GTK Conventions

Code interacting with GObject/GTK must follow the project's established patterns.

Pass conditions:

- GObject subclasses use the standard `mod imp` pattern with `#[derive(Properties)]` and `glib::wrapper!`.
- Widgets with UI use composite templates; the `.ui` file lives alongside the Rust implementation in `src/widgets/`.
- Template callbacks are connected with `#[gtk::template_callbacks]` / `#[template_callback]`.
- `glib::clone!` always uses the attribute syntax (`#[weak]`, `#[strong]`, `#[weak(rename_to=...)]`); no closure captures that create reference cycles (e.g. strong `self` captures in long-lived signal handlers).
- UI state is exposed as bindable GObject properties and updated via property notifications, not manual widget poking spread across the codebase.
