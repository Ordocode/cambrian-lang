#!/usr/bin/env python3
# Copyright (C) 2025-2026 The Cambrian Authors
# SPDX-License-Identifier: GPL-3.0-only

"""Second-pass PRE-1 fixups after initial sweep."""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
TEST_DIR = ROOT / "cambrian-transpiler" / "tests"

MATCH_INNER_GUARD = re.compile(
    r"Expr::IntLiteral\(v\) if v == (U256::[^(]+\([^)]*\))"
)

MATCH_PATTERN_INT = re.compile(
    r"MatchPattern::IntLiteral\((?!U256::)(\d+)\)"
)

HEX_MULTILINE = re.compile(
    r'Expr::HexLiteral\(\s*\n\s*"([0-9a-fA-F_]+)"\.into\(\),\s*\n\s*\)',
    re.MULTILINE,
)

HEX_REPEAT_ANY = re.compile(
    r'Expr::HexLiteral\("([a-z])"\.repeat\((\d+)\)\)'
)


def hex_expr(digits: str) -> str:
    digits = digits.replace("_", "")
    if len(digits) > 64:
        digits = digits[-64:]
    return f'Expr::IntLiteral(U256::from_hex_digits("{digits}").unwrap())'


def fix_file(path: Path) -> bool:
    text = path.read_text()
    original = text

    text = HEX_MULTILINE.sub(lambda m: hex_expr(m.group(1)), text)
    text = HEX_REPEAT_ANY.sub(
        lambda m: hex_expr(m.group(1) * (2 * int(m.group(2)))), text
    )
    text = MATCH_PATTERN_INT.sub(
        lambda m: f"MatchPattern::IntLiteral(U256::from_u128({m.group(1)}))", text
    )

    # matches!(..., Expr::IntLiteral(v) if v == U256::...) → outer guard
    def fix_inner_guard(m: re.Match) -> str:
        rhs = m.group(1)
        return f"Expr::IntLiteral(v)) if *v == {rhs}"

    # Only inside matches! — crude but works for our tests
    text = re.sub(
        r"matches!\(([^;]*?)Expr::IntLiteral\(v\) if v == (U256::[^)]+)\)",
        lambda m: f"matches!({m.group(1)}Expr::IntLiteral(v)) if *v == {m.group(2)}",
        text,
    )

    if "HexLiteral(_)" in text:
        text = text.replace(
            "assert!(matches!(value, Expr::HexLiteral(_)));",
            "assert!(matches!(value, Expr::IntLiteral(_)));",
        )

    if text != original:
        path.write_text(text)
        return True
    return False


def main() -> int:
    n = 0
    for path in sorted(TEST_DIR.rglob("*.rs")):
        if path.name.endswith(".orig"):
            continue
        if fix_file(path):
            print(path.relative_to(ROOT))
            n += 1
    print(f"fixed {n} files")
    return 0


if __name__ == "__main__":
    sys.exit(main())
