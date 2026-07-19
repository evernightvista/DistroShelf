# DistroShelf Copilot Instructions

## Project Overview
DistroShelf is a Rust-based GTK4/Libadwaita GUI for managing [Distrobox](https://distrobox.it/) containers. Built with Meson, it provides container lifecycle management, package installation, and application export functionality.

## Architecture Overview

### State Management: `RootStore` + `MainStore`
The app state is split across two GObjects, each with a different lifecycle:

**`RootStore`** (`src/models/root_store.rs`) — top-level, app-wide state. Created once per window in `DistroShelfApplication::recreate_window`, lives for the lifetime of that window. Holds:
- Host-touching abstractions (cloned handles): `command_runner`, `settings`, `file_system`, `terminal_repository`
- Cross-cutting async state: `container_runtime` (`Query<DetectedRuntime>`), `distrobox_version` / `host_distrobox_version` / `bundled_distrobox_version` (`Query<Option<DistroboxExecutable>>` / `Query<Option<VersionedExecutable>>`)
- Task management: `tasks: TypedListStore<DistroboxTask>`
- Current view + dialog (`ViewType`, `DialogType`, `DialogParams`)
- The active `MainStore` (below) when in `ViewType::Main`; set to `None` in `ViewType::Welcome`.

**`MainStore`** (`src/models/main_store.rs`) — per-view state for `ViewType::Main`. Lazily created by `RootStore::set_current_view(ViewType::Main)` and dropped when switching back to `Welcome`. Holds:
- The `Distrobox` backend handle (with a `CmdFactory` that reads the resolved `distrobox_version`)
- Container/images queries: `containers_query`, `images_query`, `downloaded_images_query`
- `TypedListStore<Container>` + GTK selection/sort models (`SingleSelection`, `SortListModel`)
- `stale_containers` (distrobox-init migration state)

`RootStore` exposes thin delegating methods (`containers()`, `selected_container()`, `load_containers()`, …) so widgets can ignore the split and call `root_store` directly. New app-wide state goes in `RootStore`; new Main-view-only state goes in `MainStore`. UI binds to GObject properties on either via data binding — never duplicate state in widgets.

### Nullable Fakers (Host-Touching Abstractions)
**Every operation that touches the host — running a process, reading/writing a file, reading/writing GSettings — MUST go through the corresponding faker in `src/fakers/`.** Each faker is a clonable `enum { Real(...), Null(...) }`:

| Faker | Real variant | Rule |
|-------|--------------|------|
| `CommandRunner` (`src/fakers/command_runner.rs`) | Spawns via `async_process` | No `std::process::Command`, `async_process::Command`, or `tokio::process::Command` outside this module |
| `FileSystem` (`src/fakers/file_system.rs`) | Forwards to `std::fs` | No `std::fs::*` outside this module (and its tests) |
| `Settings` (`src/fakers/settings.rs`) | Wraps `gio::Settings` (`com.ranfdev.DistroShelf`) | No direct `gio::Settings` outside this module |

**Why:** DistroShelf runs both natively and inside a Flatpak sandbox. The faker indirection lets (a) `Real` variants be wrapped for Flatpak (`CommandRunner::map_cmd(flatpak::map_flatpak_spawn_host)`), and (b) every host-touching code path be exercised by tests and UI previews against the `Null` variants. The `DistroboxStoreTy` enum in `src/application.rs` selects the faker variants per window — `Real` for production, `Null*` for previews/e2e tests.

**Construction:** Fakers are constructed in `DistroShelfApplication::recreate_window` based on `DistroboxStoreTy`, then passed into `RootStore::new(command_runner, settings, file_system)`. From there they propagate by cloning: `RootStore` hands them to `MainStore`, `TerminalRepository`, etc. Never call `gio::Settings::new`, `FileSystem::new_real`, etc. in widget/dialog code — get the already-constructed instance from the `RootStore`.

**Builders for tests/previews:** `NullCommandRunnerBuilder`, `NullFileSystemBuilder`, `NullSettingsBuilder` configure the `Null` variants with predetermined responses/files/values. `NullSettingsBuilder::new()` starts pre-filled with the schema defaults (`data/com.ranfdev.DistroShelf.gschema.xml`) — keep the two in sync when adding keys.

```rust
// CORRECT
let fs = root_store.file_system();
let contents = fs.read_to_string(&path)?;
let settings = root_store.settings();
let sort = settings.string("sort-key");

// WRONG - bypasses the faker, breaks in tests/Flatpak previews
let contents = std::fs::read_to_string(&path)?;
let settings = gio::Settings::new("com.ranfdev.DistroShelf");
```

### Command Value Type & `OutputTracker`
Beyond the `CommandRunner` itself, `src/fakers/` also provides:
- **`Command`** (`src/fakers/command.rs`): the project's own clonable, owned, transformable command value. `std::process::Command` isn't `Clone` and hides its stdio config; this one exposes `program`, `args`, `stdin`/`stdout`/`stderr` (`FdMode`) as public fields so commands can be passed around, cloned, and rewritten (`extend`, `map_cmd`, `remove_flag_arg`, …). Always build commands with `Command::new` / `Command::new_with_args`.
- **`OutputTracker<CommandRunnerEvent>`** (`src/fakers/output_tracker.rs`): every `runner.spawn(...)` / `runner.output(...)` pushes a `Spawned` / `Started` / `Output` event onto the runner's tracker (opt-in via `runner.output_tracker().enable()`). Tests and previews assert on the command stream rather than mocking globals.

```rust
let runner = NullCommandRunnerBuilder::new()
    .cmd(&["distrobox", "ls", "--no-color"], "ID | NAME | ...")
    .build();
let mapped = runner.map_cmd(|cmd| { /* rewrite before exec, e.g. flatpak-spawn wrap */ cmd });
let tracker = runner.output_tracker(); // .enable() called implicitly
let _ = block_on(runner.output(Command::new_with_args("distrobox", ["ls"])));
assert!(tracker.items().iter().any(|ev| matches!(ev, CommandRunnerEvent::Started(_, _))));
```

### GObject Subclassing Pattern
Standard gtk-rs pattern used throughout (`src/container.rs`, `src/window.rs`, etc.):
```rust
mod imp {
    #[derive(Properties)]
    #[properties(wrapper_type = super::MyWidget)]
    pub struct MyWidget {
        #[property(get, set)]
        name: RefCell<String>,
    }
}
glib::wrapper! {
    pub struct MyWidget(ObjectSubclass<imp::MyWidget>);
}
```

### Composite Template Pattern
UI widgets use GTK composite templates:
```rust
#[derive(gtk::CompositeTemplate)]
#[template(file = "window.ui")]
pub struct DistroShelfWindow {
    #[template_child]
    pub sidebar_list_view: TemplateChild<gtk::ListView>,
}
// Connect callbacks in imp module:
#[gtk::template_callbacks]
impl WelcomeView {
    #[template_callback]
    fn continue_to_terminal_page(&self, _: &gtk::Button) { /* ... */ }
}
```
Widget `.ui` files live alongside their Rust implementations in `src/widgets/`. Global UI resources (help overlay, etc.) remain in `data/gtk/`.

## Key Patterns & Utilities

### `Query<T>` - Async Data Fetching
Wraps async operations with reactive state (`src/query/mod.rs`). State is split across three orthogonal axes: **loading** (`is-loading`), **data presence** (`data()`), and **last-fetch outcome** (`last_fetch()` → `Pending` / `Success` / `Error`, kept independent so a failed refresh still shows stale `data`).

```rust
let query = Query::new("containers", || async { fetch_containers().await })
    .with_timeout(Duration::from_secs(5))
    .with_retry_strategy(|n| if n < 3 { Some(Duration::from_secs(n as u64)) } else { None });

query.refetch(); // Triggers fetch, updates is-loading/data/error properties
query.connect_success(|data| { /* UI update */ });
query.connect_error(|error| { /* error UI */ });
```

Properties: `is-loading`, `is-success`, `is-error`, `error-message`. Signals: `success`, `error`. Methods also expose `data()`, `age()`, `is_stale(max_age)`, `last_success_at()`.

Beyond direct fetchers, prefer the combinators over hand-rolling reactive wiring:
- **`Query::pure(value)` / `Query::pending()`** — synchronous / never-loading queries. Used as terminals for derived chains.
- **`set_fetcher`** — install the fetcher after construction (used when the fetcher needs handles that aren't available at `Query::new` time, e.g. inside `RootStore::new`).
- **`switch_map(f)`** — switch-style derived query: each source success replaces the inner query, aborting any in-flight fetch. Used to derive `distrobox_version` from the host/bundled version queries and the selected source.
- **`refetch_if_stale(max_age)` / `is_stale(max_age)`** — staleness is measured from `last_success_at`, *not* from the last attempt or last failure. A failure does not reset the staleness clock.
- **`set_refetch_strategy(...)`** with `Query::immediate` / `Query::debounce(d)` / `Query::throttle(d, trailing)` — controls how repeated `refetch()` calls collapse. `containers_query` is throttled to 1s so podman-event bursts don't flood distrobox.
- **`refetch_on(max_age, connect)` / `refetch_on_focus(window, max_age)`** — register external event sources (window focus, signal handlers, etc.) that trigger a staleness-gated refetch; teardown is owned by the query via `RefetchTriggerGuard`.

### `DistroboxTask` - Long-Running Operations
Tracks command execution with output streaming (`src/distrobox_task.rs`):
```rust
let task = DistroboxTask::new("my-container", "Upgrade", |task| async move {
    let child = runner.spawn(Command::new("distrobox-upgrade"))?;
    task.handle_child_output(child).await?; // Streams output to task.vte_terminal()
    Ok(())
});
// Status: "pending" -> "executing" -> "successful"/"failed"
// Displayed in TaskManagerDialog with live output
```
Tasks are created via `RootStore::create_task(name, action, operation)`, which appends them to `RootStore::tasks` and triggers `main.load_containers()` after they finish. Use `view_task(&task)` to switch to the `TaskManager` dialog; use `create_task` rather than constructing `DistroboxTask` directly so the post-task container refresh is wired up.

### `TypedListStore<T>` & List Reconciliation
Type-safe wrapper over `gio::ListStore` (`src/gtk_utils/typed_list_store.rs`):
```rust
let store = TypedListStore::<Container>::new();
for container in store.iter() { /* No downcasting needed */ }
```
Use `reconcile_list_by_key` to diff-update lists without full rebuild:
```rust
reconcile_list_by_key(&store, &new_containers, |c| c.name(), &["status", "image"]);
// Updates existing items, adds new, removes old - preserves object identity
```

### `glib::clone!` Macro
**Always use attribute syntax** for weak/strong references:
```rust
btn.connect_clicked(clone!(
    #[weak(rename_to=this)]
    self,
    #[strong]
    data,
    move |_| { this.do_something(&data); }
));
```

## Critical Integration Points

### Flatpak Detection
App automatically detects the Flatpak environment and configures `CommandRunner`:
- Native: Uses `CommandRunner::new_real()`
- Flatpak: Wraps the real runner with `command_runner.map_cmd(backends::flatpak::map_flatpak_spawn_host)` so every command is rewritten to run via `flatpak-spawn --host`
- See `src/backends/flatpak.rs` and `DistroShelfApplication::recreate_window` in `src/application.rs`

### Container Runtime Abstraction
`ContainerRuntime` trait (`src/backends/container_runtime.rs`) abstracts Podman/Docker:
- Auto-detects available runtime at startup
- Provides unified interface for images, events, container status
- `RootStore::container_runtime` is a `Query<DetectedRuntime>` (the detected runtime plus the version string obtained during detection)

### Desktop File Parsing
Shell script in `src/backends/distrobox/POSIX_FIND_AND_CONCAT_DESKTOP_FILES.sh` finds and encodes desktop files from containers for app export. Uses hex-encoding to avoid shell escaping issues.

## Tools

### GNOME SDK Docs Skill
Use the `gnome-sdk-docs` skill for guidance on browsing GObject Introspection (`.gir`) files, D-Bus interfaces, and icon discovery for GNOME library development.

### Subagent Tool
Use the `subagent` tool to delegate heavy tasks to specialized agents with isolated context. Available agents: `scout` (fast codebase recon), `planner` (implementation plans), `reviewer` (code review), `worker` (general-purpose). Supports single, parallel, and chained modes.

### Version Control System
jujutsu is the VCS used to manage the project. Run `jj -h` to see the full list of commands supported by the version installed.

We follow the Scoped Commits standard. Scoped Commits is a loose standard for formatting commit messages that focuses on making the commit log quickly understandable to contributors.

Normal commit messages should be formatted as follows:

```
<scope>: <description>

[optional body]

[optional trailer(s)]
```

where:
- <scope> — the subsystem, area, or module that the commit touches
- <description> — a short description of the changes made
- [optional body] — detailed information about the changes
- [optional trailer(s)] — additional metadata about the commit
