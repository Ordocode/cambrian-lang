#!/usr/bin/env bash
# Copyright (C) 2025-2026 The Cambrian Authors
# SPDX-License-Identifier: GPL-3.0-only

# Byte-diff harness for EVM Solidity codegen (P5 kernel-adapter refactor).
#
# Transpiles every fixture in ALL_FIXTURES (see test_evm_forge.rs) via the
# CLI `--target evm` path and stores concatenated generated .sol output per
# fixture. Baseline and current dirs live under /tmp so goldens are never
# committed.
#
# Both address modes are snapshotted:
#   * non-deterministic — single-file CLI (`deterministic_addresses=false`)
#   * deterministic     — temp project.yaml with `deterministic_addresses: true`
#
# Usage:
#   scripts/evm_solidity_snapshot.sh snapshot   # write /tmp/cambrian-evm-p5-baseline
#   scripts/evm_solidity_snapshot.sh diff       # regenerate current, diff -ru vs baseline
#   scripts/evm_solidity_snapshot.sh            # print instructions
set -u

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
if [[ -x "$ROOT/target/release/cambrian-transpiler" ]]; then
  BIN="$ROOT/target/release/cambrian-transpiler"
else
  BIN="$ROOT/target/debug/cambrian-transpiler"
fi

BASELINE="/tmp/cambrian-evm-p5-baseline"
CURRENT="/tmp/cambrian-evm-p5-current"
FIXTURES_DIR="$ROOT/contracts"

# Mirrors cambrian-transpiler/tests/test_evm_forge.rs ALL_FIXTURES.
ALL_FIXTURES=(
  counter escrow escrow_v2 identity_vault identity_pair
  ledger receiver sender broker guardian
  shop nft registry wallet
  staking dex payment_channel example batch1_showcase
  batch1_an phased_vault phased_predictable mixed_types
  voting vault token stdlib_demo
  token_query
  loop_evm
  fold_evm
  enum_data
  extern_token_caller
  erc20_events_evm
  erc20_errors_evm
  eth_vault_evm
)

transpile_nondet() {
  local cam="$1" out_dir="$2"
  "$BIN" "$cam" -o "$out_dir" --target evm >/dev/null 2>&1
}

transpile_det() {
  local cam="$1" out_dir="$2" name="$3"
  local proj
  proj="$(mktemp -d)"
  cp "$cam" "$proj/${name}.cam"
  cat > "$proj/project.yaml" <<EOF
name: p5-snap-${name}
target: evm
deterministic_addresses: true
output_dir: out/
sources:
  - ${name}.cam
EOF
  if "$BIN" --project "$proj/project.yaml" >/dev/null 2>&1; then
    mkdir -p "$out_dir"
    if [[ -d "$proj/out" ]]; then
      cp -R "$proj/out/." "$out_dir/"
    fi
    rm -rf "$proj"
    return 0
  fi
  rm -rf "$proj"
  return 1
}

collect_sol() {
  local src="$1" dest_file="$2"
  {
    find "$src" -name '*.sol' 2>/dev/null | sort | while read -r f; do
      echo "// FILE: ${f#"$src"/}"
      cat "$f"
    done
  } > "$dest_file"
}

snapshot_dir() {
  local out="${1:?}"
  rm -rf "$out"
  mkdir -p "$out/nondet" "$out/det"

  local ok_n=0 skip_n=0 miss=0
  local ok_d=0 skip_d=0
  for name in "${ALL_FIXTURES[@]}"; do
    local cam="$FIXTURES_DIR/${name}.cam"
    if [[ ! -f "$cam" ]]; then
      miss=$((miss + 1))
      continue
    fi

    local tmp
    tmp="$(mktemp -d)"
    if transpile_nondet "$cam" "$tmp"; then
      collect_sol "$tmp" "$out/nondet/${name}.sol"
      ok_n=$((ok_n + 1))
    else
      skip_n=$((skip_n + 1))
    fi
    rm -rf "$tmp"

    tmp="$(mktemp -d)"
    if transpile_det "$cam" "$tmp" "$name"; then
      collect_sol "$tmp" "$out/det/${name}.sol"
      # Empty file means project mode wrote nothing useful — count as skip.
      if [[ ! -s "$out/det/${name}.sol" ]]; then
        rm -f "$out/det/${name}.sol"
        skip_d=$((skip_d + 1))
      else
        ok_d=$((ok_d + 1))
      fi
    else
      skip_d=$((skip_d + 1))
    fi
    rm -rf "$tmp"
  done
  echo "snapshot nondet: $ok_n transpiled, $skip_n failed, $miss missing"
  echo "snapshot det:    $ok_d transpiled, $skip_d failed -> $out"
}

usage() {
  cat <<EOF
EVM Solidity snapshot harness (P5 byte-diff).

  scripts/evm_solidity_snapshot.sh snapshot
      Write baseline to $BASELINE/{nondet,det}

  scripts/evm_solidity_snapshot.sh diff
      Regenerate into $CURRENT and run: diff -ru $BASELINE $CURRENT

  Modes:
    nondet — single-file CLI (deterministic_addresses=false)
    det    — temp project.yaml with deterministic_addresses: true

  Transpiler binary: $BIN
EOF
}

mode="${1:-}"
case "$mode" in
  snapshot)
    snapshot_dir "$BASELINE"
    ;;
  diff)
    snapshot_dir "$CURRENT"
    echo "--- diff -ru $BASELINE $CURRENT ---"
    diff -ru "$BASELINE" "$CURRENT"
    ;;
  "")
    usage
    ;;
  *)
    echo "unknown mode: $mode" >&2
    usage >&2
    exit 1
    ;;
esac
