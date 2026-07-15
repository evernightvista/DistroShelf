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
- **Correctness gain**: solving it improves software correctness (yes/no)
- **Simplicity gain**: solving it improves software simplicity and maintainability (yes/no)

Present the comments sorted by confidence, highest first.
