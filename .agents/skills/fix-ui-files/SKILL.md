---
name: fix-ui-files
description: "Diagnose and pinpoint structural errors in GTK Builder .ui files (mismatched closing tags, unclosed elements). Use when a .ui file fails to load with a Gtk-WARNING about template precompilation or tag mismatches."
---

# Fix UI Files

Use this skill when a GTK Builder `.ui` file fails to load. Typical symptoms:

- Runtime warning: `Failed to precompile template for class X: Error on line N char M: Element "A" was closed, but the currently open element is "B"`
- A blank window where a composite template widget should appear.
- `GLib-CRITICAL: g_bytes_get_size: assertion 'bytes != NULL' failed` (downstream of the parse failure).

## Diagnostic script

Run `scripts/validate_ui.py` (relative to this skill's directory) on a `.ui` file or a directory:

```sh
python3 .agents/skills/fix-ui-files/scripts/validate_ui.py src/widgets/window.ui
# or, to scan everything:
python3 .agents/skills/fix-ui-files/scripts/validate_ui.py src/
```

The script walks the file's element stack tag-by-tag and pinpoints:

1. The line where the wrong closing tag was found.
2. The line where the still-open tag was opened (i.e. what should have been closed).
3. Context snippets (surrounding lines) around both.
4. A suggested fix.

## Common causes

The most frequent class of error is editing the widget tree (wrapping or unwrapping a layer) and forgetting to update closing tags:

- **Adding a wrapper** (e.g. inserting a `GtkBox` between an overlay and its child) without adding the matching `</object>` and `</child>`.
- **Removing a wrapper** (e.g. deleting the `GtkBox` that stacked a banner above a split view) but leaving its closing tags in place. The extra `</object>` / `</child>` cascade into mismatches for every outer scope beyond them.
- **Renaming an element** in the opening tag but forgetting the closing tag.

## Verification

Re-run the script after editing to confirm the file is clean:

```sh
python3 .agents/skills/fix-ui-files/scripts/validate_ui.py src/
```

Then run `cargo build` (which compiles composite templates for `#[template(file = "...")]`-wired Rust structs) and launch the application. A parse failure surfaces as a `Gtk-WARNING` at startup, not as a Rust compile error.

## When NOT to use

- **`Failed to find object 'X' in template`** — a `TemplateChild<>` field references an `id` that does not exist in the `.ui` file. Not a structural XML problem; grep for the id in the `.ui` file.
- **CSS issues, GObject property typos, missing `<requires>`** — these do not surface as XML parse failures and need a running application to detect.
