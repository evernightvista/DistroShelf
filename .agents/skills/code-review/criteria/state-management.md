# Criterion: State Management

App state must flow through the central reactive store.

Pass conditions:

- New app-level state lives in `RootStore` (`src/store/root_store.rs`) and is exposed as bindable properties; widgets bind to it instead of holding duplicated state.
- Async data fetching uses `Query<T>` (`src/query/mod.rs`) so loading/error states come for free, instead of ad-hoc spawned futures mutating widgets directly.
- Lists use `TypedListStore<T>` and are updated with `reconcile_list_by_key` (diff-update) rather than clearing and rebuilding, preserving object identity.
- Long-running operations are modeled as `DistroboxTask` with proper status transitions (pending → executing → successful/failed).
