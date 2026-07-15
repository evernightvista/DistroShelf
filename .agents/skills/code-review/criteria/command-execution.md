# Criterion: Command Execution

All shell command execution MUST go through the `CommandRunner` abstraction (`src/fakers/command_runner.rs`).

Pass conditions:

- No direct use of `std::process::Command` (or `tokio`/`async-process` equivalents) outside of `CommandRunner` implementations.
- Commands are built with the project's `Command` type and executed via `runner.output(...)`, `runner.spawn(...)`, etc.
- Code does not assume it runs on the host: it must also work under Flatpak, where commands are wrapped with `flatpak-spawn --host`.
- Long-running commands with user-visible output are wrapped in a `DistroboxTask` so output is streamed to the task terminal.
