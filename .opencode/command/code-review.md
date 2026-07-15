---
description: Review code changes against the project's review criteria, producing scored review comments.
agent: general
subtask: true
---

You are performing a code review. Load the `code-review` skill with the skill tool and follow its instructions exactly.

Code under review: $ARGUMENTS

If no code was specified above, review the current uncommitted changes (`jj diff`).
