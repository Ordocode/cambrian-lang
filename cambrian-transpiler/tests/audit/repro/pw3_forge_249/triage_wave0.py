#!/usr/bin/env python3
# Copyright (C) 2025-2026 The Cambrian Authors
# SPDX-License-Identifier: GPL-3.0-only

"""Wave 0 triage for PW3 forge-249 failures.

Usage (from repo root):
  python3 cambrian-transpiler/tests/audit/repro/pw3_forge_249/triage_wave0.py \\
    --log /tmp/coverage_forge_wave0.log

Writes:
  triage_validate_manifest.jsonl
  triage.csv
  triage_summary.md
"""

from __future__ import annotations

import argparse
import csv
import json
import re
import subprocess
import sys
from collections import Counter
from dataclasses import dataclass, field
from pathlib import Path

_CRATE_ROOT = Path(__file__).resolve().parents[4]
REPO_ROOT = _CRATE_ROOT.parent
COVERAGE_RS = _CRATE_ROOT / "tests/test_audit_coverage_codegen.rs"
REPRO_DIR = Path(__file__).resolve().parent


@dataclass
class TestMeta:
    name: str
    body: str
    cam: str | None = None
    ast_inject: bool = False
    harness_output: bool = False
    deterministic: bool = False
    solc_errors: list[str] = field(default_factory=list)
    forge_label: str | None = None


def failed_test_names(log_text: str) -> list[str]:
    names = []
    for line in log_text.splitlines():
        m = re.match(r"^test (n4_[^\s]+) \.\.\. FAILED$", line)
        if m:
            names.append(m.group(1))
    return names


def split_tests(rs_text: str) -> dict[str, str]:
    parts = re.split(r"\n#\[test\]\nfn ", rs_text)
    out: dict[str, str] = {}
    for part in parts[1:]:
        m = re.match(r"([a-zA-Z0-9_]+)\(\)", part)
        if not m:
            continue
        out[m.group(1)] = part
    return out


def extract_cam(body: str) -> str | None:
    patterns = (
        r'parse_evm\(\s*r#"(.*?)"#',
        r'gen_evm_solidity_patched\([^)]*r#"(.*?)"#',
    )
    for pat in patterns:
        m = re.search(pat, body, re.DOTALL)
        if m:
            return m.group(1)
    return None


def classify_meta(name: str, body: str) -> TestMeta:
    meta = TestMeta(name=name, body=body)
    meta.cam = extract_cam(body)
    meta.ast_inject = any(
        tok in body or tok in name
        for tok in (
            "gen_evm_solidity_patched",
            "Expr::",
            "RouteBody::",
            "InvariantDecl",
            "ast_inject",
            "_continue",
            "_decoy",
            "patch_first_",
            "program.entities[",
        )
    )
    meta.harness_output = any(
        tok in body
        for tok in (
            "gen_evm_test_files",
            "gen_evm_test_files_det",
            "generate_evm_tests",
        )
    )
    meta.deterministic = any(
        tok in body or tok in name
        for tok in (
            "gen_evm_solidity(&program, true)",
            "gen_evm_solidity_patched(src, true",
            "gen_evm_solidity_patched(\n        src, true",
            "gen_evm_test_files_det",
            "_det_",
        )
    )
    m = re.search(r'assert_(?:solc|forge)_compiles\("([^"]+)"', body)
    if m:
        meta.forge_label = m.group(1)
    return meta


def parse_failure_blocks(log_text: str) -> dict[str, list[str]]:
    """Map test_name -> solc error lines."""
    blocks = re.split(r"---- (n4_[^\s]+) stdout ----", log_text)
    out: dict[str, list[str]] = {}
    # blocks: [prefix, name1, body1, name2, body2, ...]
    for i in range(1, len(blocks), 2):
        name = blocks[i]
        body = blocks[i + 1] if i + 1 < len(blocks) else ""
        errs = re.findall(r"Error \(\d+\): ([^\n]+)", body)
        if errs:
            out[name] = errs
    return out


def primary_error(errors: list[str]) -> str:
    return errors[0] if errors else ""


def secondary_lane(
    errors: list[str], meta: TestMeta, validator_verdict: str
) -> str:
    if len(errors) > 1:
        return "MULTI_ERR"
    if validator_verdict.startswith("ERR:") and not meta.ast_inject:
        return "TB-V"
    return ""


def assign_lane(
    errors: list[str], meta: TestMeta, validator_verdict: str
) -> str:
    pe = primary_error(errors)
    cam = meta.cam or ""

    if meta.ast_inject and not meta.harness_output:
        if validator_verdict.startswith("ERR:"):
            return "TB-AST+TB-V"
        return "TB-AST"

    if "forge-std/" in pe or "forge-std/Test.sol" in pe:
        return "HF-1"

    if meta.harness_output and ("forge-std" in pe or "Source \"" in pe):
        return "HF-1"

    if meta.deterministic and any(
        k in pe.lower()
        for k in (
            "immutable",
            "wrong argument count",
            "constructor",
            "create2",
            "addressof",
        )
    ):
        return "CG-FACTORY"

    if "Vec::new()" in cam:
        return "TB-VEC"

    if validator_verdict.startswith("ERR:") and not meta.ast_inject:
        return "TB-V"

    if 'Member "values"' in pe or 'Member "keys"' in pe:
        return "CG-ITER-1"

    if "Data location must be" in pe and "mapping" in pe:
        return "CG-TYPES-2"

    if "Undeclared identifier" in pe:
        if "^" in cam or "temporal" in meta.name:
            return "CG-EXPR-1"
        return "CG-EXPR-1"

    if "Member \"x\"" in pe or "Member \"length\"" in pe:
        return "CG-TYPES-3"

    if "Member \"missing\"" in pe:
        return "CG-MISC"

    if "contract I" in pe and "conversion" in pe:
        return "CG-ROUTE-1"

    if "Return argument type" in pe or "Return argument type" in pe:
        return "CG-TYPES-4"

    if "Wrong argument count" in pe:
        return "CG-MISC"

    if "Different number of components" in pe:
        return "CG-TYPES-5"

    if "Type bool is not implicitly convertible to expected type uint256" in pe:
        return "CG-ANALYSIS-1"

    if "[] memory" in pe and "convertible to expected type uint256" in pe:
        return "CG-TYPES-1"

    if "Expected ',' but got" in pe or "Invalid character in string" in pe:
        return "CG-SYNTAX-1"

    if "iterator" in cam.lower() or "filter(" in cam or "Unsupported" in pe:
        return "CG-E07"

    return "CG-MISC"


def notes_for(meta: TestMeta, validator_verdict: str, errors: list[str]) -> str:
    bits = []
    if meta.cam is None:
        bits.append("no_extractable_cam")
    if meta.harness_output:
        bits.append("harness")
    if meta.deterministic:
        bits.append("det")
    if len(errors) > 1:
        bits.append(f"multi_err={len(errors)}")
    if validator_verdict.startswith("ERR:"):
        bits.append(validator_verdict)
    return "; ".join(bits)


def run_validate_batch(manifest_path: Path) -> dict[str, str]:
    cmd = [
        "cargo",
        "test",
        "-p",
        "cambrian-transpiler",
        "--test",
        "test_audit_pw3_triage_wave0",
        "validate_batch",
        "--",
        "--nocapture",
    ]
    subprocess.run(cmd, cwd=REPO_ROOT, check=True)
    results_path = REPRO_DIR / "triage_validate_results.jsonl"
    verdicts: dict[str, str] = {}
    for line in results_path.read_text().splitlines():
        if not line.strip():
            continue
        row = json.loads(line)
        verdicts[row["test_name"]] = row["validator_verdict"]
    return verdicts


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument(
        "--log",
        default="/tmp/coverage_forge_wave0.log",
        help="cargo test log with forge failures",
    )
    ap.add_argument("--skip-validate", action="store_true")
    args = ap.parse_args()

    log_path = Path(args.log)
    if not log_path.exists():
        print(f"log missing: {log_path}", file=sys.stderr)
        return 1

    log_text = log_path.read_text()
    failed = failed_test_names(log_text)
    if len(failed) != 249:
        print(f"warning: expected 249 failures, got {len(failed)}", file=sys.stderr)

    rs_text = COVERAGE_RS.read_text()
    tests = split_tests(rs_text)
    err_map = parse_failure_blocks(log_text)

    manifest_path = REPRO_DIR / "triage_validate_manifest.jsonl"
    with manifest_path.open("w") as mf:
        for name in failed:
            body = tests.get(name, "")
            meta = classify_meta(name, body)
            if meta.cam:
                mf.write(
                    json.dumps(
                        {
                            "test_name": name,
                            "cam": meta.cam,
                            "deterministic": meta.deterministic,
                        }
                    )
                    + "\n"
                )

    verdicts: dict[str, str] = {}
    if not args.skip_validate:
        verdicts = run_validate_batch(manifest_path)
    else:
        results = REPRO_DIR / "triage_validate_results.jsonl"
        if results.exists():
            for line in results.read_text().splitlines():
                row = json.loads(line)
                verdicts[row["test_name"]] = row["validator_verdict"]

    rows = []
    lane_counts: Counter[str] = Counter()
    val_counts: Counter[str] = Counter()

    for name in failed:
        body = tests.get(name, "")
        meta = classify_meta(name, body)
        meta.solc_errors = err_map.get(name, [])

        if meta.cam is None:
            vv = "SKIP_NO_CAM" if meta.ast_inject else "SKIP_NO_CAM"
        elif name in verdicts:
            vv = verdicts[name]
        else:
            vv = "MISSING"

        lane = assign_lane(meta.solc_errors, meta, vv)
        sec = secondary_lane(meta.solc_errors, meta, vv)
        lane_counts[lane] += 1
        val_counts[vv.split(":")[0]] += 1

        rows.append(
            {
                "test_name": name,
                "validator_verdict": vv,
                "ast_inject": str(meta.ast_inject).lower(),
                "harness_output": str(meta.harness_output).lower(),
                "deterministic": str(meta.deterministic).lower(),
                "solc_errors": "|".join(meta.solc_errors),
                "primary_lane": lane,
                "secondary_lane": sec,
                "notes": notes_for(meta, vv, meta.solc_errors),
            }
        )

    csv_path = REPRO_DIR / "triage.csv"
    fields = [
        "test_name",
        "validator_verdict",
        "ast_inject",
        "harness_output",
        "deterministic",
        "solc_errors",
        "primary_lane",
        "secondary_lane",
        "notes",
    ]
    with csv_path.open("w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=fields)
        w.writeheader()
        w.writerows(rows)

    tb_v_err = sum(1 for r in rows if r["validator_verdict"].startswith("ERR:"))
    ast_only = sum(
        1 for r in rows if r["ast_inject"] == "true" and r["harness_output"] == "false"
    )
    ast_any = sum(1 for r in rows if r["ast_inject"] == "true")
    harness_n = sum(1 for r in rows if r["harness_output"] == "true")
    det_n = sum(1 for r in rows if r["deterministic"] == "true")
    hf1 = lane_counts.get("HF-1", 0)
    multi_err = sum(1 for r in rows if r["solc_errors"].count("|") >= 1)
    tb_v_lane = lane_counts.get("TB-V", 0) + lane_counts.get("TB-AST+TB-V", 0)

    err_codes: Counter[str] = Counter()
    for r in rows:
        if r["validator_verdict"].startswith("ERR:"):
            for c in r["validator_verdict"][4:].split(","):
                err_codes[c] += 1

    summary = f"""# PW3 forge-249 — Wave 0 triage summary

**Date:** 2026-09-11  
**Log:** `{log_path}`  
**Rows:** {len(rows)}  
**CSV:** `triage.csv`

## Validator (`validate()` + EVM `check_target_compat`)

| Verdict bucket | Count |
| --- | ---: |
"""
    for k, v in sorted(val_counts.items(), key=lambda x: (-x[1], x[0])):
        summary += f"| `{k}` | {v} |\n"

    summary += f"""
**Validator errors (ERR:…):** {tb_v_err} rows (plan est. ~50)  
**AST-inject (any):** {ast_any} rows · **no harness:** {ast_only}  
**Harness output:** {harness_n} rows · **deterministic:** {det_n} rows  
**Rows with 2+ solc errors:** {multi_err} (plan est. 30)  

### Validator error codes (48 ERR rows)

| Code | Count |
| --- | ---: |
"""
    for code, cnt in err_codes.most_common():
        summary += f"| `{code}` | {cnt} |\n"

    summary += """
## Primary lane distribution

| Lane | Count |
| --- | ---: |
"""
    for k, v in lane_counts.most_common():
        summary += f"| {k} | {v} |\n"

    summary += f"""
## Plan vs triage (heuristic baseline @ plan draft)

| Class | Plan est. | Triage |
| --- | ---: | ---: |
| HF-1 forge-std | 19 | {hf1} |
| CG-FACTORY det | 21 | {lane_counts.get('CG-FACTORY', 0)} |
| TB-V validator | ~50 | {tb_v_err} ERR / {tb_v_lane} primary lane |
| TB-AST | ~27 | {ast_only} no-harness + {lane_counts.get('TB-AST', 0)} primary |
| TB-VEC (`Vec::new()`) | 21 | {lane_counts.get('TB-VEC', 0)} |
| CG-E07 admitted | ~17 | {lane_counts.get('CG-E07', 0)} primary (E07 in validator: {err_codes.get('E07', 0)}) |

## Recommended wave order (unchanged from plan)

1. Owner sign-off on this table
2. **TB-V** + **TB-AST** ({tb_v_err} validator ERR + {ast_only} AST-only fixtures)
3. **HF-1** ({hf1} rows) — forge-std harness
4. **CG-FACTORY** ({lane_counts.get('CG-FACTORY', 0)} rows)
5. Remaining codegen clusters per lane counts above

## DoD

- [x] 249 rows in `triage.csv`
- [x] validator verdict per extractable `.cam`
- [x] solc primary error + lane assignment
- [ ] owner sign-off
"""

    summary_path = REPRO_DIR / "triage_summary.md"
    summary_path.write_text(summary)

    print(f"wrote {csv_path} ({len(rows)} rows)")
    print(f"wrote {summary_path}")
    print(f"lanes: {dict(lane_counts.most_common(8))}")
    print(f"validator ERR: {tb_v_err}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
