# Criterion: Nullable Fakers (Host-Touching Abstractions)

Every operation that touches the host — running a process, touching the filesystem, reading/writing GSettings — MUST go through the corresponding nullable faker in `src/fakers/`. Each faker is a clonable `enum { Real(...), Null(...) }`, and `DistroboxStoreTy` selects the variant per window (`Real` for production, `Null*` for previews/e2e tests).

This is non-negotiable: the same code path must work natively, under Flatpak, and in tests/previews. Bypassing a faker is a regression — it silently breaks the sandbox model and the test harness.

## Fakers and their rules

- **`CommandRunner`** (`src/fakers/command_runner.rs`): no `std::process::Command`, `async_process::Command`, or `tokio::process::Command` outside `src/fakers/`. Spawn via `runner.spawn(...)` or `runner.output(...)` / `runner.output_string(...)`.
- **`FileSystem`** (`src/fakers/file_system.rs`): no `std::fs::*` outside `src/fakers/file_system.rs` (and its own tests). Read/write/create/remove/rename through `root_store.file_system()`.
- **`Settings`** (`src/fakers/settings.rs`): no `gio::Settings::new(...)` or direct `gio::Settings` construction outside `src/fakers/`. Read/write through `root_store.settings()` (`string`, `set_string`, `boolean`, `int`, …).

## Command value type

- Commands are built with the project's own `Command` (`src/fakers/command.rs`) — `Command::new(program)` / `Command::new_with_args(program, args)`. Never use `std::process::Command`.
- `Command` exposes `program`, `args`, `stdin`/`stdout`/`stderr` (`FdMode`) as public fields; rewrite with `extend`, `map_cmd`, `remove_flag_arg`, … rather than rebuilding.

## Construction and propagation

- Fakers are constructed in `DistroShelfApplication::recreate_window` (selected by `DistroboxStoreTy`) and passed into `RootStore::new(command_runner, settings, file_system)`. From there they propagate by cloning to `MainStore`, `TerminalRepository`, etc.
- Widget/dialog/test code MUST obtain the already-constructed faker from the `RootStore` (`root_store.command_runner()`, `root_store.settings()`, `root_store.file_system()`), not construct a new one. New `gio::Settings::new`, `FileSystem::new_real`, etc. inside widget/dialog code fails this criterion.
- `NullSettingsBuilder::new()` starts pre-filled with the schema defaults in `data/com.ranfdev.DistroShelf.gschema.xml`. Adding a schema key without updating the builder's defaults fails this criterion.

## Output tracking

- Every `runner.spawn(...)` / `runner.output(...)` pushes an event onto the runner's `OutputTracker` (opt-in via `runner.output_tracker().enable()`). Tests and previews assert on the command stream (`CommandRunnerEvent::Spawned` / `Started` / `Output`) rather than mocking globals or environment variables.

## Long-running commands

- Commands with user-visible streamed output MUST be wrapped in a `DistroboxTask` (via `RootStore::create_task`) so output is streamed to the task terminal, not lost.
