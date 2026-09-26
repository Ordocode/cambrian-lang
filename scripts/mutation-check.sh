#!/usr/bin/env bash
# Copyright (C) 2025-2026 The Cambrian Authors
# SPDX-License-Identifier: GPL-3.0-only

#
# mutation-check.sh — the alarm test for an EVM-target Cambrian suite.
#
#   scripts/mutation-check.sh stdlib/vault
#
# A property suite that has never caught anything is indistinguishable from
# one that cannot. On the Lean target that check is written as `.mut.cam`:
# deliberately false claims that `runMutation*` inverts, so a counterexample
# is the passing outcome. Projects that cannot be lowered to Lean — anything
# reaching another contract, because Lean P3 wants `rescue` on a capture from
# a failing route and `rescue` is not supported on that target (L12) — have no
# such surface. This is the substitute, and it is the stronger claim of the
# two: instead of asking whether the sampler can falsify a false statement, it
# breaks the implementation and asks whether the existing suite notices.
#
# Each mutant is one literal substitution in one source file, listed in
# `<project>/mutants.txt`:
#
#   id | file | search | replace
#
# The search text must appear EXACTLY ONCE in the file. A mutant whose search
# text has drifted out of the source is a hard error, not a skip — a mutation
# suite that silently stops applying its mutants is the failure mode it exists
# to prevent.
#
# Exit status is 0 when every mutant was caught, 1 when any survived or any
# mutant failed to apply.

set -uo pipefail

PROJECT="${1:-}"
if [ -z "$PROJECT" ]; then
    echo "usage: $0 <project-dir> [mutant-id ...]" >&2
    exit 2
fi

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROJECT_ABS="$(cd "$PROJECT" && pwd)"
PROJECT_NAME="$(basename "$PROJECT_ABS")"
MUTANTS_FILE="$PROJECT_ABS/mutants.txt"
if [ -n "${CAMBRIAN_TRANSPILER:-}" ]; then
  TRANSPILER="$CAMBRIAN_TRANSPILER"
else
  RELEASE_BIN="$REPO_ROOT/target/release/cambrian-transpiler"
  DEBUG_BIN="$REPO_ROOT/target/debug/cambrian-transpiler"
  if [ -x "$RELEASE_BIN" ] && { [ ! -x "$DEBUG_BIN" ] || [ "$RELEASE_BIN" -nt "$DEBUG_BIN" ]; }; then
    TRANSPILER="$RELEASE_BIN"
  else
    TRANSPILER="$DEBUG_BIN"
  fi
fi

shift || true
WANTED=("$@")

if [ ! -f "$MUTANTS_FILE" ]; then
    echo "no mutants.txt in $PROJECT_ABS" >&2
    exit 2
fi
if [ ! -x "$TRANSPILER" ]; then
    echo "transpiler not built: $TRANSPILER" >&2
    exit 2
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# The vault project reaches one directory up for `../token/ERC20Multi.cam` and
# `../math.cam`, so the copy has to preserve the parent layout rather than the
# project directory alone. Drop sibling `build` / `build-lean` trees so a
# prior forge run is not copied eleven times.
mkdir -p "$WORK/parent"
cp -R "$(dirname "$PROJECT_ABS")/." "$WORK/parent"
SANDBOX="$WORK/parent/$PROJECT_NAME"
rm -rf "$WORK/parent"/*/build "$WORK/parent"/*/build-lean "$SANDBOX/build"

caught=0
survived=0
failed=0
declare -a SURVIVORS=()

while IFS='|' read -r id file search replace; do
    # Skip comments and blank lines.
    case "${id// /}" in ''|'#'*) continue ;; esac

    id="$(echo "$id" | xargs)"
    file="$(echo "$file" | xargs)"

    if [ ${#WANTED[@]} -gt 0 ]; then
        match=0
        for w in "${WANTED[@]}"; do [ "$w" = "$id" ] && match=1; done
        [ $match -eq 1 ] || continue
    fi

    printf '── %-28s ' "$id"

    # Restore the pristine source, then apply exactly this one mutation.
    cp "$PROJECT_ABS/$file" "$SANDBOX/$file"

    if ! SEARCH="$search" REPLACE="$replace" python3 - "$SANDBOX/$file" <<'PY'
import os, sys
path = sys.argv[1]
search = os.environ["SEARCH"].strip()
replace = os.environ["REPLACE"].strip()
text = open(path).read()
n = text.count(search)
if n != 1:
    sys.stderr.write(f"\nsearch text occurs {n} times (want exactly 1) in {path}:\n  {search}\n")
    sys.exit(1)
open(path, "w").write(text.replace(search, replace))
PY
    then
        echo "MUTANT DID NOT APPLY"
        failed=$((failed + 1))
        continue
    fi

    log="$WORK/$id.log"
    (
        cd "$SANDBOX" || exit 1
        "$TRANSPILER" build --project project.yaml >"$log" 2>&1 || exit 1
        cd build || exit 1
        # Foundry's dependency tree is 60 MB of git checkout and identical for
        # every mutant, so it is borrowed from the pristine build rather than
        # re-installed eleven times.
        mkdir -p lib
        [ -e lib/forge-std ] || ln -sfn "$PROJECT_ABS/build/lib/forge-std" lib/forge-std
        [ -d lib/forge-std/src ] || { echo "forge-std missing" >>"$log"; exit 90; }
        forge test >>"$log" 2>&1
    )
    rc=$?

    # 90 is the harness failing to set itself up, which must never be read as
    # a caught mutant — that is exactly the false green this suite exists to
    # rule out.
    if [ $rc -eq 90 ]; then
        echo "HARNESS BROKEN (see $log)"
        cp "$log" "/tmp/mutation-$id.log" 2>/dev/null || true
        failed=$((failed + 1))
        continue
    fi

    # Non-zero is the outcome we want: the transpiler refused the mutant, or
    # Foundry reported a failing test. Either way the suite noticed.
    #
    # An id prefixed `noop-` inverts the expectation: it is a control that
    # changes nothing observable, and a harness that reports it as caught is
    # reporting on something other than the mutation.
    case "$id" in
        noop-*)
            if [ $rc -eq 0 ]; then
                echo "control survived (as it must)"
                caught=$((caught + 1))
            else
                echo "CONTROL WAS CAUGHT — the harness is measuring noise"
                cp "$log" "/tmp/mutation-$id.log" 2>/dev/null || true
                failed=$((failed + 1))
            fi
            ;;
        *)
            if [ $rc -ne 0 ]; then
                echo "caught"
                caught=$((caught + 1))
            else
                echo "SURVIVED"
                survived=$((survived + 1))
                SURVIVORS+=("$id")
            fi
            ;;
    esac
done < "$MUTANTS_FILE"

# Leave the tree as we found it.
cp "$PROJECT_ABS"/*.cam "$SANDBOX"/ 2>/dev/null || true

echo
echo "caught $caught · survived $survived · did not apply $failed"
if [ ${#SURVIVORS[@]} -gt 0 ]; then
    echo "survivors: ${SURVIVORS[*]}"
fi
[ $survived -eq 0 ] && [ $failed -eq 0 ]
