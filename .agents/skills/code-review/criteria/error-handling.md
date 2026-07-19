# Criterion: Error Handling

Errors must be handled deliberately, never silently swallowed or allowed to crash the UI.

Pass conditions:

- No `unwrap()`/`expect()` on fallible operations in non-test code paths that can realistically fail at runtime (I/O, command execution, parsing external data). `expect()` with a justification is acceptable only for programmer invariants.
- Errors are propagated with `?` or surfaced to the user (task failure state, `Query::connect_error` / `is-error` / `error-message` axis, dialog/toast), not dropped with `let _ =` or empty match arms.
- Failures of external commands (distrobox, podman/docker) are handled: non-zero exit codes and unparseable output must not panic.
- Error messages shown to users are actionable and include relevant context.
