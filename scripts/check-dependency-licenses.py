#!/usr/bin/env python3
# Copyright (C) 2025-2026 The Cambrian Authors
# SPDX-License-Identifier: GPL-3.0-only
"""Fail-loud license audit of the cargo dependency tree (CI `legal` gate).

Reads `cargo metadata` (no extra tooling) and checks:
  - every workspace member declares `license = "GPL-3.0-only"`
    (except the private `tvm-cell` fork, which is unpublished third-party code);
  - every external dependency's SPDX expression is satisfiable with
    GPL-3.0-compatible permissive licenses only.

An unknown license or a new SPDX expression that cannot be satisfied from the
allowlist fails the job; extend ALLOWED deliberately after a human review,
never to silence the gate.
"""

import json
import re
import subprocess
import sys

# GPL-3.0-compatible permissive licenses we accept for dependencies.
ALLOWED = {
    "MIT",
    "Apache-2.0",
    "Apache-2.0 WITH LLVM-exception",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "0BSD",
    "Unlicense",
    "CC0-1.0",
    "Zlib",
    "BSL-1.0",
    "Unicode-3.0",
    "Unicode-DFS-2016",
}

WORKSPACE_LICENSE = "GPL-3.0-only"

# Workspace members allowed to have no `license` field.
UNLICENSED_MEMBERS = {
    # Private fork of TON Labs tvm_types (Apache-2.0 upstream, see
    # tvm-cell/NOTICE); kept unpublished, not part of the public tree.
    "tvm-cell",
}


def split_top_level(expr: str, sep: str) -> list[str]:
    """Split on `sep` outside parentheses."""
    parts, depth, start = [], 0, 0
    i = 0
    while i < len(expr):
        c = expr[i]
        if c == "(":
            depth += 1
        elif c == ")":
            depth -= 1
        elif depth == 0 and expr.startswith(sep, i):
            parts.append(expr[start:i])
            i += len(sep)
            start = i
            continue
        i += 1
    parts.append(expr[start:])
    return parts


def expr_allowed(expr: str) -> bool:
    """True iff every top-level AND part has at least one allowed OR alternative.

    Handles the SPDX subset cargo uses in practice, plus the legacy `/`
    separator (treated as OR, per cargo's historical convention).
    """
    expr = expr.strip()
    for part in split_top_level(expr, " AND "):
        part = part.strip()
        while part.startswith("(") and part.endswith(")"):
            part = part[1:-1].strip()
        alternatives = re.split(r"\s+OR\s+|\s*/\s*", part)
        if not any(alt.strip() in ALLOWED for alt in alternatives):
            return False
    return True


def main() -> int:
    meta = json.loads(
        subprocess.run(
            ["cargo", "metadata", "--format-version", "1"],
            check=True,
            capture_output=True,
        ).stdout
    )
    workspace_ids = set(meta["workspace_members"])

    failures = []
    checked_deps = 0
    for pkg in meta["packages"]:
        name, version, license_ = pkg["name"], pkg["version"], pkg.get("license")
        if pkg["id"] in workspace_ids:
            if name in UNLICENSED_MEMBERS:
                continue
            if license_ != WORKSPACE_LICENSE:
                failures.append(
                    f"workspace member {name} declares {license_!r}, "
                    f"expected {WORKSPACE_LICENSE!r}"
                )
            continue
        checked_deps += 1
        if not license_:
            failures.append(
                f"dependency {name} {version} has no `license` field "
                f"(license_file: {pkg.get('license_file')!r}) — review manually"
            )
        elif not expr_allowed(license_):
            failures.append(
                f"dependency {name} {version} license {license_!r} is not "
                f"satisfiable from the allowlist"
            )

    if failures:
        print(f"FAIL: {len(failures)} license problem(s):", file=sys.stderr)
        for f in failures:
            print(f"  - {f}", file=sys.stderr)
        return 1
    print(
        f"OK: {len(workspace_ids)} workspace members GPL-3.0-only "
        f"(+{len(UNLICENSED_MEMBERS)} exempt), "
        f"{checked_deps} external dependencies within the allowlist"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
