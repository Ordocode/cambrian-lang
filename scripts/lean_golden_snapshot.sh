#!/usr/bin/env bash
# Copyright (C) 2025-2026 The Cambrian Authors
# SPDX-License-Identifier: GPL-3.0-only

# Parity snapshot for the Lean route-codegen IR refactor (Layer 3).
#
# Transpiles every standalone .cam under contracts/ and examples/ to Lean
# and concatenates the generated .lean into one text file per source under
# the snapshot dir. Run before and after the refactor; `diff -r` the two
# dirs to prove the IR rewrite is behaviour-preserving.
#
# Usage: scripts/lean_golden_snapshot.sh <out-dir>
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/debug/cambrian-transpiler"
OUT="${1:?usage: lean_golden_snapshot.sh <out-dir>}"
rm -rf "$OUT"
mkdir -p "$OUT"

# Collect candidate standalone sources (skip .fuzz/.invariant fragments).
ok=0; skipped=0
while IFS= read -r cam; do
  rel="${cam#"$ROOT"/}"
  safe="${rel//\//__}"
  tmp="$(mktemp -d)"
  if "$BIN" "$cam" -o "$tmp" --target lean >/dev/null 2>&1; then
    {
      # Deterministic concatenation of all generated .lean files.
      find "$tmp" -name '*.lean' | sort | while read -r f; do
        echo "-- FILE: ${f#"$tmp"/}"
        cat "$f"
      done
    } > "$OUT/$safe.lean"
    ok=$((ok+1))
  else
    skipped=$((skipped+1))
  fi
  rm -rf "$tmp"
done < <(find "$ROOT/contracts" "$ROOT/examples" -name '*.cam' \
  ! -name '*.fuzz.cam' ! -name '*.invariant.cam' | sort)
echo "snapshot: $ok transpiled, $skipped skipped (validator-gated/non-standalone) -> $OUT"
