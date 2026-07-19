# Criterion: State Management

App state must flow through the reactive store split: `RootStore` (app-wide) and `MainStore` (per Main view).

Pass conditions:

- New app-wide state lives in `RootStore` (`src/models/root_store.rs`) and is exposed as bindable GObject properties; widgets bind to it instead of holding duplicated state. App-wide means anything that needs to outlive the Main view (host-touching faker handles, distrobox version resolution, container runtime detection, tasks, dialogs, current view).
- New Main-view-only state (containers, images, selection/sort models, distrobox-init staleness) lives in `MainStore` (`src/models/main_store.rs`). It is created lazily when entering `ViewType::Main` and dropped when switching to `Welcome`, so anything stored there must tolerate being recreated.
- Widgets call the thin delegating methods on `RootStore` (`containers()`, `selected_container()`, `load_containers()`, …) rather than reaching into a `MainStore` directly. Direct `MainStore` access outside `RootStore` fails this criterion unless the widget is itself scoped to the Main view's lifetime.
- Async data fetching uses `Query<T>` (`src/query/mod.rs`) so loading/error states come for free, instead of ad-hoc spawned futures mutating widgets directly. Prefer the combinators (`pure`, `pending`, `set_fetcher`, `switch_map`, `refetch_if_stale`, `set_refetch_strategy`, `refetch_on_focus`) over hand-rolling reactive wiring.
- Lists use `TypedListStore<T>` and are updated with `reconcile_list_by_key` (diff-update) rather than clearing and rebuilding, preserving object identity.
- Long-running operations are modeled as `DistroboxTask`s created via `RootStore::create_task` (not `DistroboxTask::new` directly), so the post-task container refresh is wired up.
