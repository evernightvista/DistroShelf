---
name: code-review
description: "Use when performing a code review of changes, a diff, a commit, or a merge request in this project. Defines the review procedure and the criteria (in criteria/) the code must be evaluated against."
---

# Code Review

You MUST:

1. Read every file in `criteria/` (relative to this skill's directory).
2. Evaluate the code under review against each criterion.
3. Report findings as review comments.

Each review comment MUST include the location (`file:line`), a description of the issue, and this set of scores:

- **Confidence**: how confident the issue actually exists (0%-100%)
- **Priority**: P1 (highest) to P5 (lowest), denoting how important it is to fix the issue. P1 = must fix (bugs, data loss, crashes), P2 = should fix before merge, P3 = worth fixing, P4 = minor improvement, P5 = nitpick.

Present the comments sorted by priority (P1 first), then by confidence, highest first.

## Be critical

Review with a skeptical, adversarial mindset — assume the code has problems and hunt for them. Do NOT settle for a surface-level pass:

- Trace the actual data and control flow of the changed code; do not just read it line by line. Follow the code paths into callers and callees outside the diff when needed.
- Actively look for: race conditions, unhandled error paths, incorrect edge cases (empty lists, None/null, unicode, concurrency), leaked resources, broken invariants, regressions in behavior the old code had.
- Question the design, not just the implementation: is this change necessary? Is there a simpler approach? Does it duplicate existing utilities in the codebase?
- Verify claims: if a comment, commit message, or name implies a behavior, check the code actually does that.
- For each changed area, ask "how could this break?" and write down at least one hypothesis before concluding the area is fine.
- A review that finds no P1-P3 issues in a non-trivial change is suspicious; re-examine before concluding the code is clean.

Do not pad the review with speculative or trivial comments to appear thorough — every reported issue must be concrete and actionable.
