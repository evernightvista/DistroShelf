---
name: release
description: 'Prepare the next release of DistroShelf: version bump, release notes, commit, and tag.'
---

# Release Process

Your task is to prepare the next release for the DistroShelf project:

1. **Analyze changes**: Retrieve all commits since the last tag using `git log --oneline --reverse $(git describe --tags --abbrev=0)..HEAD`. Ensure you get the full list (not truncated).

2. **Categorize changes**: From commit messages, identify user-visible changes and classify them using the existing release note convention from `data/com.ranfdev.DistroShelf.metainfo.xml.in`:
   - `New:` — new features
   - `Improved:` — enhancements to existing features
   - `Fix:` — bug fixes
   - `Changed:` — behavior changes or removals
   - `i18n:` — translation updates (only note new languages)
   
   Ignore internal refactors, dependency bumps, documentation-only changes, and other non-user-facing changes.

3. **Write release notes**: Append the new release entry at the **top** of the `<releases>` section in `data/com.ranfdev.DistroShelf.metainfo.xml.in`. Use the existing format:
   ```xml
   <release version="X.Y.Z" date="YYYY-MM-DD">
     <description translate="no">
       <ul>
         <li>New: Feature description.</li>
         <li>Fix: Bug description.</li>
       </ul>
     </description>
   </release>
   ```

4. **Determine version**: Follow semver (major.minor.patch) based on the types of changes:
   - New features → minor bump
   - Breaking changes → major bump
   - Only fixes → patch bump

5. **Update version**: Bump the `version:` field in `meson.build` (line 2) to match.

6. **Commit & Tag**: Stage changes, commit with message `vX.Y.Z`, and create an annotated tag `vX.Y.Z`:
   ```
   git add -A && git commit -m "vX.Y.Z"
   git tag vX.Y.Z
   ```
