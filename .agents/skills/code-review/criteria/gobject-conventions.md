# Criterion: GObject and GTK Conventions

Code interacting with GObject/GTK must follow the project's established patterns.

Pass conditions:

- GObject subclasses use the standard `mod imp` pattern with `#[derive(Properties)]` and `glib::wrapper!`.
- Runtime-injected dependencies (faker handles, `Distrobox`, models, sorters, selection models) are stored in `OnceCell` fields and installed once during construction (`.set(...).expect("… already set")`). Mutable per-instance state that lives in the GObject goes in `RefCell` (or `Cell` for `Copy` types).
- Widgets with UI use composite templates; the `.ui` file lives alongside the Rust implementation in `src/widgets/`.
- Template callbacks are connected with `#[gtk::template_callbacks]` / `#[template_callback]`.
- `glib::clone!` always uses the attribute syntax (`#[weak]`, `#[strong]`, `#[weak(rename_to=...)]`); no closure captures that create reference cycles (e.g. strong `self` captures in long-lived signal handlers). Prefer capturing `weak(rename_to = obj)` clones of the `RootStore`/`MainStore`, or downgrade and re-upgrade inside the closure when the lifetime is unclear.
- UI state is exposed as bindable GObject properties and updated via property notifications, not manual widget poking spread across the codebase.
