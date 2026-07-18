---
name: code-review
description: "Use when performing a code review of changes, a diff, a commit, or a merge request in this project. Defines the review procedure and the criteria (in criteria/) the code must be evaluated against."
---

# Code Review

You MUST:

1. Read every file in `criteria/` (relative to this skill's directory).
2. Evaluate the code under review against each criterion.
3. Report findings as review comments.
4. Record future improvements in `.opencode/state/future-improvements.md` (see "Future improvements" below).

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

## Future improvements

After reporting the review comments, record *future improvements* in `.opencode/state/future-improvements.md` (create the file and directory if missing).

A future improvement is an observation about the project as a whole, discovered while reviewing the diff, that does **not** block the diff. The diff is the probe; the codebase is the patient. The change under review may be perfectly fine — the insight is about what the change *revealed*.

**Litmus test:** "Would I ask the author to fix this in *this* change?" If no, but the project should address it eventually → future improvement. If yes → regular review comment. Never mix the two channels.

Signals to capture:

- **friction** — the change required more work than the intent justified (touched many files for one feature, repeated boilerplate, manual sync of duplicated state). Ask "why was this hard?"
- **missing-abstraction** — the diff duplicates logic because no shared helper exists; a refactor would make this whole class of change trivial.
- **recurring-nit** — the same P4/P5 comment keeps appearing across reviews; it's systemic, not incidental. Candidate for a new criterion, a lint, or a refactor.
- **verification-gap** — behavior couldn't be confirmed because there's no test harness/preview/mock for that area.
- **architecture** — the change is fine now, but a few more changes like it will strain the current design. Name the tipping point.

Procedure:

1. Read the existing `.opencode/state/future-improvements.md` first.
2. If an existing entry already covers an idea, update that entry by appending the new evidence instead of duplicating it — recurring evidence is what promotes an idea from "hunch" to "do it".
3. Otherwise append a new entry in this format:

```markdown
## <short title>
- Date / review context: <date>, <what was being reviewed>
- Signal: friction | missing-abstraction | recurring-nit | verification-gap | architecture
- Evidence: <files/observations that triggered this>
- Idea: <suggested direction, not a full plan>
```

It is acceptable to record nothing when the review genuinely surfaced no project-level insight — do not invent entries to appear thorough. At the end of the review, mention (in one line) which future improvements were recorded or updated, if any.
