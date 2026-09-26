#!/usr/bin/env python3
# Copyright (C) 2025-2026 The Cambrian Authors
# SPDX-License-Identifier: GPL-3.0-only

"""Mechanical PRE-1 sweep: Expr::IntLiteral(u128) / HexLiteral / BinLiteral → IntLiteral(U256)."""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
TEST_DIR = ROOT / "cambrian-transpiler" / "tests"

INT_LIT_RE = re.compile(
    r"Expr::IntLiteral\((?!U256::)(\d+(?:_\d+)*|0x[0-9a-fA-F_]+)\)"
)

MATCHES_INT_RE = re.compile(
    r"matches!\([^)]*Expr::IntLiteral\((?!U256::)(\d+(?:_\d+)*)\)"
)

HEX_STATIC_0X_RE = re.compile(
    r'Expr::HexLiteral\(\s*"0x([0-9a-fA-F_]+)"(?:\.to_string\(\)|\.into\(\))?\s*\)'
)

HEX_STATIC_RE = re.compile(
    r'Expr::HexLiteral\(\s*"([0-9a-fA-F_]+)"(?:\.to_string\(\)|\.into\(\))?\s*\)'
)

HEX_EMPTY_RE = re.compile(r"Expr::HexLiteral\(String::new\(\)\)")

HEX_REPEAT_RE = re.compile(
    r'Expr::HexLiteral\("([ab])"\s*\.repeat\((\d+)\)\)'
)

BIN_LIT_NUM_RE = re.compile(r"Expr::BinLiteral\((\d+(?:_\d+)*|0x[0-9a-fA-F_]+)\)")


def hex_to_u256_expr(digits: str) -> str:
    digits = digits.replace("_", "")
    if not digits:
        return "Expr::IntLiteral(U256::ZERO)"
    if len(digits) > 64:
        digits = digits[-64:]
    return f'Expr::IntLiteral(U256::from_hex_digits("{digits}").unwrap())'


def int_token_to_u256(token: str) -> str:
    token = token.replace("_", "")
    if token.startswith(("0x", "0X")):
        val = int(token, 16)
    else:
        val = int(token, 10)
    if val == 0:
        return "Expr::IntLiteral(U256::ZERO)"
    return f"Expr::IntLiteral(U256::from_u128({val}))"


def fix_matches_int(content: str) -> str:
    def repl(m: re.Match) -> str:
        full = m.group(0)
        num = m.group(1)
        val = int(num.replace("_", ""), 10)
        return full.replace(
            f"Expr::IntLiteral({num})",
            f"Expr::IntLiteral(v) if v == U256::from_u128({val})",
        )

    return MATCHES_INT_RE.sub(repl, content)


def ensure_u256_import(content: str) -> str:
    if "cambrian_core::U256" in content:
        return content
    if "U256::" not in content:
        return content
    lines = content.splitlines(keepends=True)
    insert_at = 0
    for i, line in enumerate(lines):
        stripped = line.strip()
        if stripped.startswith("//!") or stripped.startswith("#["):
            insert_at = i + 1
            continue
        if stripped.startswith("use ") or stripped == "":
            insert_at = i + 1
            continue
        break
    lines.insert(insert_at, "use cambrian_core::U256;\n")
    return "".join(lines)


def transform(content: str) -> str:
    content = HEX_EMPTY_RE.sub("Expr::IntLiteral(U256::ZERO)", content)

    def repeat_sub(m: re.Match) -> str:
        ch, n = m.group(1), int(m.group(2))
        return hex_to_u256_expr(ch * (2 * n))

    content = HEX_REPEAT_RE.sub(repeat_sub, content)
    content = HEX_STATIC_0X_RE.sub(lambda m: hex_to_u256_expr(m.group(1)), content)
    content = HEX_STATIC_RE.sub(lambda m: hex_to_u256_expr(m.group(1)), content)
    content = BIN_LIT_NUM_RE.sub(lambda m: int_token_to_u256(m.group(1)), content)
    content = fix_matches_int(content)
    content = INT_LIT_RE.sub(lambda m: int_token_to_u256(m.group(1)), content)
    return ensure_u256_import(content)


def main() -> int:
    changed = 0
    for path in sorted(TEST_DIR.rglob("*.rs")):
        if path.name.endswith(".orig"):
            continue
        original = path.read_text()
        if (
            "HexLiteral" not in original
            and "BinLiteral" not in original
            and not INT_LIT_RE.search(original)
            and not MATCHES_INT_RE.search(original)
        ):
            continue
        new = transform(original)
        if new != original:
            path.write_text(new)
            print(f"updated {path.relative_to(ROOT)}")
            changed += 1
    print(f"done: {changed} files")
    return 0


if __name__ == "__main__":
    sys.exit(main())
