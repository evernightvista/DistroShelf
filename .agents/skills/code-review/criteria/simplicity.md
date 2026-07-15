# Criterion: Simplicity and Maintainability

Changes must keep the codebase simple and consistent.

Pass conditions:

- No dead code, unused imports, or commented-out blocks left behind.
- No unnecessary abstraction: new traits, generics, or indirection layers must be justified by at least two concrete users.
- Duplicated logic is factored into existing utilities (`src/gtk_utils/`, `src/query/`, etc.) instead of being copy-pasted.
- Naming, module placement, and style match the surrounding code.
- New dependencies are only added when the standard library or existing dependencies cannot reasonably do the job.
- Comments are absent unless they explain non-obvious *why*; no comments that restate the code.
