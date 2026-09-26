#!/usr/bin/env python3
# Copyright (C) 2025-2026 The Cambrian Authors
# SPDX-License-Identifier: GPL-3.0-only
"""Stamp Cambrian copyright + GPL-3.0-only SPDX on tracked source files.

Default is a dry run. Does not write unless you pass --apply.

    scripts/add-copyright-headers.py
    scripts/add-copyright-headers.py --apply

Header:

    Copyright (C) 2025-2026 The Cambrian Authors
    SPDX-License-Identifier: GPL-3.0-only

Skipped (not ours, generated, or not source):
  tvm-cell/, examples/**/ref/, examples/reference/, **/goldens/,
  LICENSE, JSON (no comments), files that already have this notice,
  files with a different SPDX, files with a third-party Copyright line.
"""

from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

COPYRIGHT = "Copyright (C) 2025-2026 The Cambrian Authors"
SPDX = "SPDX-License-Identifier: GPL-3.0-only"

# Comment prefix by suffix. Suffixes not listed are ignored.
SLASH_EXTS = {".rs", ".lalrpop", ".js", ".ts", ".cam", ".sol", ".c", ".h"}
HASH_EXTS = {
    ".py",
    ".sh",
    ".toml",
    ".yml",
    ".yaml",
    ".makefile",
}
LEAN_EXTS = {".lean"}

SKIP_PREFIXES = (
    "tvm-cell/",
    "examples/reference/",
)

SKIP_PATH_PARTS = (
    "/ref/",
    "/goldens/",
    "/generated/",
)

# Exact relative paths (git ls-files style).
SKIP_FILES = {
    "LICENSE",
}

# If any of these appear in the first N lines, do not overwrite.
THIRD_PARTY_MARKERS = (
    "TON Labs",
    "EverX",
    "Uniswap",
    "OpenZeppelin",
    "GOSH",
    "Free Software Foundation",
)

HEAD_BYTES = 4096


def git_ls_files() -> list[str]:
    out = subprocess.check_output(
        ["git", "ls-files", "-z"],
        cwd=ROOT,
        text=True,
    )
    return [p for p in out.split("\0") if p]


def comment_style(rel: str) -> str | None:
    name = Path(rel).name
    if name in ("Makefile", "Makefile.am", "GNUmakefile") or name.startswith(
        "Dockerfile"
    ):
        return "hash"
    suffix = Path(rel).suffix.lower()
    if suffix in SLASH_EXTS:
        return "slash"
    if suffix in HASH_EXTS:
        return "hash"
    if suffix in LEAN_EXTS:
        return "lean"
    if name.endswith(".toml"):
        return "hash"
    return None


def should_skip_path(rel: str) -> str | None:
    if rel in SKIP_FILES:
        return "skip-list"
    if rel.startswith(SKIP_PREFIXES):
        return "path prefix"
    posix = "/" + rel.replace("\\", "/")
    for part in SKIP_PATH_PARTS:
        if part in posix:
            return f"path contains {part.strip('/')}"
    if Path(rel).suffix.lower() == ".json":
        return "json has no comments"
    if comment_style(rel) is None:
        return "unsupported type"
    return None


def detect_newline(data: bytes) -> str:
    return "\r\n" if b"\r\n" in data else "\n"


def head_text(data: bytes) -> str:
    return data[:HEAD_BYTES].decode("utf-8", errors="replace")


def already_ours(head: str) -> bool:
    return COPYRIGHT in head and "GPL-3.0-only" in head


def existing_spdx(head: str) -> str | None:
    for line in head.splitlines()[:30]:
        s = line.strip().lstrip("/*#- ").strip()
        if s.startswith("SPDX-License-Identifier:"):
            ident = s.split(":", 1)[1].strip()
            return ident
    return None


def third_party(head: str) -> str | None:
    for line in head.splitlines()[:40]:
        low = line.lower()
        if "copyright" not in low and "©" not in line:
            continue
        for marker in THIRD_PARTY_MARKERS:
            if marker in line:
                return marker
    return None


def header_lines(style: str) -> list[str]:
    if style == "slash":
        prefix = "// "
    elif style == "hash":
        prefix = "# "
    elif style == "lean":
        prefix = "-- "
    else:
        raise ValueError(style)
    return [f"{prefix}{COPYRIGHT}", f"{prefix}{SPDX}"]


def split_preamble(text: str) -> tuple[str, str]:
    """Keep shebang and an optional Emacs/Python encoding cookie first."""
    lines = text.splitlines(keepends=True)
    if not lines:
        return "", ""
    i = 0
    if lines[0].startswith("#!"):
        i = 1
    if i < len(lines):
        stripped = lines[i].lstrip()
        if stripped.startswith("#") and (
            "coding:" in stripped or "coding=" in stripped
        ):
            i += 1
    return "".join(lines[:i]), "".join(lines[i:])


def leading_spdx(text: str) -> bool:
    """True when the body already opens with an SPDX line (ignore blanks)."""
    for line in text.lstrip("\r\n").splitlines()[:8]:
        stripped = line.strip()
        if not stripped:
            continue
        ident = stripped.lstrip("/*#- ").strip()
        return ident.startswith("SPDX-License-Identifier:")
    return False


def insert_header(text: str, style: str, newline: str) -> str:
    preamble, rest = split_preamble(text)
    lines = header_lines(style)
    # A file that already declares GPL-3.0-only still needs the copyright
    # line, but a second SPDX identifier makes solc reject the file.
    if leading_spdx(rest):
        lines = lines[:1]
    block = newline.join(lines) + newline
    if rest.startswith("\r\n") or rest.startswith("\n"):
        body = rest
    elif rest:
        body = newline + rest
    else:
        body = newline
    if preamble and not preamble.endswith(("\n", "\r\n")):
        preamble += newline
    return preamble + block + body


def classify(rel: str, data: bytes) -> tuple[str, str]:
    """Return (action, note). action is add | skip."""
    why = should_skip_path(rel)
    if why:
        return "skip", why
    if not data:
        return "skip", "empty"
    head = head_text(data)
    if already_ours(head):
        return "skip", "already stamped"
    ident = existing_spdx(head)
    if ident and ident != "GPL-3.0-only":
        return "skip", f"existing SPDX {ident}"
    marker = third_party(head)
    if marker:
        return "skip", f"third-party ({marker})"
    return "add", comment_style(rel) or "slash"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--apply",
        action="store_true",
        help="write headers (default: print what would change)",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="exit 1 if any file would be stamped (CI)",
    )
    args = parser.parse_args()

    try:
        files = git_ls_files()
    except (OSError, subprocess.CalledProcessError) as e:
        print(f"git ls-files failed: {e}", file=sys.stderr)
        return 2

    add: list[tuple[str, str]] = []
    skipped: list[tuple[str, str]] = []

    for rel in files:
        path = ROOT / rel
        if not path.is_file():
            continue
        data = path.read_bytes()
        action, note = classify(rel, data)
        if action == "add":
            add.append((rel, note))
        else:
            skipped.append((rel, note))

    mode = "APPLY" if args.apply else "DRY-RUN"
    print(f"{mode}: {len(add)} to stamp, {len(skipped)} skipped")
    print()
    for rel, style in add:
        print(f"  ADD  [{style:5}] {rel}")
    reasons: dict[str, int] = {}
    for _, note in skipped:
        reasons[note] = reasons.get(note, 0) + 1
    print("\nskipped by reason:")
    for note, n in sorted(reasons.items(), key=lambda kv: (-kv[1], kv[0])):
        print(f"  {n:5}  {note}")
    if args.apply:
        for rel, style in add:
            path = ROOT / rel
            data = path.read_bytes()
            newline = detect_newline(data)
            try:
                text = data.decode("utf-8")
            except UnicodeDecodeError:
                print(f"  FAIL non-utf8 {rel}", file=sys.stderr)
                continue
            new = insert_header(text, style, newline)
            path.write_bytes(new.encode("utf-8"))
        print(f"\nwrote {len(add)} files")

    if args.check and add:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
