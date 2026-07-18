#!/usr/bin/env python3
"""Validate GTK Builder .ui files and pinpoint XML structural errors.

Specifically tailored for the common failure mode where editing the widget tree
(wrapping/unwrapping layers) leaves mismatched closing tags. Reports:

  - Where the mismatched closing tag is.
  - Where the unclosed opening tag originated.
  - Context snippets around both.
  - A suggested fix.

Usage:
    python3 validate_ui.py [file_or_dir ...]

If no arguments are given, scans the current directory recursively for *.ui files.
Exit code: 0 if all files parse cleanly, 1 if any error is found.
"""

import re
import sys
import xml.parsers.expat
from pathlib import Path

# ---------------------------------------------------------------------------
# Pre-processing -- strip comments while preserving byte offsets
# ---------------------------------------------------------------------------
COMMENT_RE = re.compile(rb"<!--.*?-->", re.DOTALL)


def strip_comments(data: bytes) -> bytes:
    """Replace XML comments with whitespace of the same shape, preserving
    newlines, so byte positions and line/col of real tags stay correct."""

    def _repl(m):
        return bytes(b"\n" if c == 0x0A else 0x20 for c in m.group(0))

    return COMMENT_RE.sub(_repl, data)


# ---------------------------------------------------------------------------
# Tag extraction
# ---------------------------------------------------------------------------
TAG_RE = re.compile(rb"<(/?)([A-Za-z_:][\w:.-]*)([^>]*?)(/?)>")


def _line_col(data: bytes, offset: int):
    """Return (lineno, col) 1-indexed for *offset* (byte) in *data*."""
    lineno = data.count(b"\n", 0, offset) + 1
    line_start = data.rfind(b"\n", 0, offset) + 1
    return lineno, offset - line_start


def walk_tags(data: bytes):
    """Yield (kind, name, lineno, col, raw_text, self_closing) for every
    non-PI, non-DOCTYPE tag.  *kind* is ``'open'`` or ``'close'``."""
    data = strip_comments(data)
    for m in TAG_RE.finditer(data):
        slash, name_bytes, _attrs, self_close = (
            m.group(1),
            m.group(2),
            m.group(3),
            m.group(4),
        )
        name = name_bytes.decode("utf-8")
        if name.startswith("?") or name.startswith("!"):
            continue
        lineno, col = _line_col(data, m.start())
        yield (
            "close" if slash == b"/" else "open",
            name,
            lineno,
            col,
            m.group(0).decode("utf-8", "replace"),
            self_close == b"/",
        )


# ---------------------------------------------------------------------------
# File helpers
# ---------------------------------------------------------------------------
def read_lines(path: Path) -> list[str]:
    with open(path, encoding="utf-8") as f:
        return f.read().splitlines(keepends=False)


# ---------------------------------------------------------------------------
# Diagnostics
# ---------------------------------------------------------------------------
CONTEXT_SPAN = 2


def format_context(lines: list[str], lineno: int) -> str:
    """Return `CONTEXT_SPAN` lines above and below *lineno* (1-indexed)."""
    start = max(1, lineno - CONTEXT_SPAN)
    end = min(len(lines), lineno + CONTEXT_SPAN)
    out: list[str] = []
    for i in range(start, end + 1):
        marker = ">>" if i == lineno else "  "
        out.append(
            "  {marker} {ln:>5}: {text}".format(
                marker=marker, ln=i, text=lines[i - 1]
            )
        )
    return "\n".join(out)


def explain(path: Path) -> str | None:
    """Return ``None`` if the file is well-formed, else a detailed diagnostic."""
    lines = read_lines(path)
    raw = path.read_bytes()

    # Quick check: does expat even see a problem?
    try:
        xml.parsers.expat.ParserCreate().Parse(raw, True)
        return None
    except xml.parsers.expat.ExpatError:
        pass  # fall through to stack-walker

    # Walk tags and find the **first** mismatch.
    stack: list[tuple[str, int, int, str]] = []  # (name, lineno, col, raw)
    for kind, name, lineno, col, raw_text, self_close in walk_tags(raw):
        if kind == "open":
            if self_close:
                continue
            stack.append((name, lineno, col, raw_text))
            continue

        # kind == 'close'
        if not stack:
            return _fmt_spurious_close(path, lines, name, lineno, col)
        top_name, top_line, top_col, _ = stack[-1]
        if top_name == name:
            stack.pop()
            continue

        # Mismatch!
        return _fmt_mismatch(
            path,
            lines,
            found=name,
            found_lineno=lineno,
            found_col=col,
            expected=top_name,
            expected_lineno=top_line,
            expected_col=top_col,
        )

    if stack:
        name, lineno, col, _ = stack[-1]
        return _fmt_unclosed_eof(path, lines, name, lineno, col)

    # Shouldn't reach here -- expat thinks there is an error.
    try:
        xml.parsers.expat.ParserCreate().Parse(raw, True)
    except xml.parsers.expat.ExpatError as e:
        return "  {path}:{lineno}:{col}: expat error (stack balanced): {err}".format(
            path=path,
            lineno=e.lineno,
            col=(e.offset or 0),
            err=e,
        )
    return None


def _fmt_mismatch(
    path: Path,
    lines: list[str],
    *,
    found: str,
    found_lineno: int,
    found_col: int,
    expected: str,
    expected_lineno: int,
    expected_col: int,
) -> str:
    return (
        "\n"
        "{path}:{line}:{col}: mismatched closing tag\n"
        "\n"
        "  Found:  </{found}>  at line {fl}\n"
        "  Expected </{expected}> to close the tag opened at line {el}:\n"
        "\n"
        "{ctx_expected}\n"
        "\n"
        "  ...but found </{found}> here instead:\n"
        "\n"
        "{ctx_found}\n"
        "\n"
        "  Suggestion: either\n"
        "    (a) change line {fl} from </{found}> to </{expected}>, or\n"
        "    (b) insert a </{expected}> on the line(s) before {fl} if a wrapper\n"
        "        layer was removed and its closing tags were missed, or\n"
        "    (c) if this is a cascade from an earlier mismatch further up the\n"
        "        file, fix the earlier one first, then re-run.\n"
    ).format(
        path=path,
        line=found_lineno,
        col=found_col + 1,
        found=found,
        fl=found_lineno,
        expected=expected,
        el=expected_lineno,
        ctx_expected=format_context(lines, expected_lineno),
        ctx_found=format_context(lines, found_lineno),
    )


def _fmt_spurious_close(
    path: Path, lines: list[str], name: str, lineno: int, col: int
) -> str:
    return (
        "\n"
        "{path}:{line}:{col}: spurious closing tag\n"
        "\n"
        "  Found </{name}> but no tags are currently open.\n"
        "\n"
        "{ctx}\n"
    ).format(
        path=path,
        line=lineno,
        col=col + 1,
        name=name,
        ctx=format_context(lines, lineno),
    )


def _fmt_unclosed_eof(
    path: Path, lines: list[str], name: str, lineno: int, col: int
) -> str:
    return (
        "\n"
        "{path}: unclosed tag at end of file\n"
        "\n"
        "  Tag <{name}> opened at line {line} was never closed.\n"
        "\n"
        "{ctx}\n"
    ).format(
        path=path,
        name=name,
        line=lineno,
        ctx=format_context(lines, lineno),
    )


# ---------------------------------------------------------------------------
# File discovery
# ---------------------------------------------------------------------------
def find_ui_files(paths: list[str]) -> list[Path]:
    result: list[Path] = []
    for p_str in paths:
        p = Path(p_str)
        if p.is_dir():
            result.extend(sorted(p.rglob("*.ui")))
        elif p.suffix == ".ui" and p.exists():
            result.append(p)
    return result


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------
def main(argv: list[str]) -> int:
    args = argv[1:]
    if not args:
        args = ["."]

    files = find_ui_files(args)
    if not files:
        print("No .ui files found.", file=sys.stderr)
        return 1

    had_error = False
    for f in files:
        diag = explain(f)
        if diag is None:
            print("OK  {path}".format(path=f))
        else:
            print(diag)
            had_error = True

    return 1 if had_error else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
