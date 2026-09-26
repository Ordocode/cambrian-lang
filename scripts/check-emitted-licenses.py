#!/usr/bin/env python3
# Copyright (C) 2025-2026 The Cambrian Authors
# SPDX-License-Identifier: GPL-3.0-only
"""Transpile a minimal project with support-file options on; check emit headers.

Used by the GitLab `legal` job. Fails if:
  - a required Lean/EVM support file is missing (options did not fire);
  - a copied support file is not tagged UNLICENSED;
  - any emitted text file still carries the repo GPL identifier.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

FORBIDDEN = ("GPL-3.0-only", "SPDX-License-Identifier: GPL")
LEAN_UNLICENSED = "-- SPDX-License-Identifier: UNLICENSED\n"
SOL_UNLICENSED = "// SPDX-License-Identifier: UNLICENSED\n"

LEAN_SUPPORT = (
    "Cambrian/Core.lean",
    "Cambrian/Evm.lean",
    "Cambrian/Prelude.lean",
    "Cambrian/SimpAttrs.lean",
    "plausible/Cambrian/Core.lean",
    "plausible/Cambrian/Evm.lean",
    "plausible/Cambrian/Prelude.lean",
    "plausible/Cambrian/SimpAttrs.lean",
    "plausible/Testing/CambrianHarness.lean",
    "plausible/Testing/Generators.lean",
)

TEXT_SUFFIXES = {".lean", ".sol", ".rs", ".toml", ".json", ".md"}


def repo_root() -> Path:
    return Path(__file__).resolve().parent.parent


def transpiler_bin() -> Path:
    env = os.environ.get("CAMBRIAN_TRANSPILER")
    if env:
        return Path(env)
    root = repo_root()
    for rel in ("target/debug/cambrian-transpiler", "target/release/cambrian-transpiler"):
        cand = root / rel
        if cand.is_file():
            return cand
    subprocess.run(
        ["cargo", "build", "-p", "cambrian-transpiler"],
        cwd=root,
        check=True,
    )
    return root / "target/debug/cambrian-transpiler"


def write_yaml(path: Path, *, target: str, output_dir: Path, lean: bool) -> None:
    lines = [
        f"name: legal-emit-{target}",
        f"target: {target}",
        f"output_dir: {output_dir}",
        "deterministic_addresses: true",
        "sources:",
        "  - legal_emit.cam",
    ]
    if lean:
        lines += [
            "lean:",
            "  emission_profile: predictable",
            "  plausible: true",
            "  intrinsics: executable",
            "  proof_helpers: true",
        ]
    path.write_text("\n".join(lines) + "\n")


def run_transpile(bin_path: Path, yaml: Path) -> None:
    r = subprocess.run(
        [str(bin_path), "--project", str(yaml)],
        cwd=repo_root(),
        capture_output=True,
        text=True,
    )
    if r.returncode != 0:
        sys.stderr.write(r.stdout)
        sys.stderr.write(r.stderr)
        raise SystemExit(f"FAIL: transpiler exited {r.returncode} for {yaml}")


def scan_tree(root: Path) -> list[str]:
    failures: list[str] = []
    for p in root.rglob("*"):
        if not p.is_file():
            continue
        if p.suffix not in TEXT_SUFFIXES and p.name != "lean-toolchain":
            continue
        text = p.read_text(errors="replace")
        rel = p.relative_to(root).as_posix()
        for token in FORBIDDEN:
            if token in text:
                failures.append(f"{rel}: contains {token!r}")
    return failures


def check_lean(out: Path) -> list[str]:
    failures: list[str] = []
    for rel in LEAN_SUPPORT:
        path = out / rel
        if not path.is_file():
            failures.append(f"missing required Lean support file: {rel}")
            continue
        text = path.read_text()
        if not text.startswith(LEAN_UNLICENSED):
            failures.append(f"{rel}: does not start with UNLICENSED SPDX")
    prelude = out / "Cambrian/Prelude.lean"
    if prelude.is_file():
        body = prelude.read_text()
        if "CAMBRIAN:PREDICTABLE:BEGIN" not in body:
            failures.append(
                "Cambrian/Prelude.lean: predictable splice missing "
                "(emission_profile: predictable did not fire)"
            )
    evm = out / "Cambrian/Evm.lean"
    if evm.is_file():
        body = evm.read_text()
        if "executable reference models" not in body:
            failures.append(
                "Cambrian/Evm.lean: executable intrinsics splice missing "
                "(lean.intrinsics: executable did not fire)"
            )
    failures.extend(scan_tree(out))
    return failures


def check_evm(out: Path) -> list[str]:
    failures: list[str] = []
    sols = list(out.rglob("*.sol"))
    if not sols:
        failures.append("EVM emit produced no .sol files")
        return failures
    saw_unlicensed = False
    saw_muldiv = False
    saw_wadd = False
    for path in sols:
        text = path.read_text()
        rel = path.relative_to(out).as_posix()
        if text.startswith(SOL_UNLICENSED):
            saw_unlicensed = True
        else:
            failures.append(f"{rel}: .sol does not start with UNLICENSED SPDX")
        if "_cam_muldiv" in text:
            saw_muldiv = True
        if "function _wadd" in text:
            saw_wadd = True
    if not saw_unlicensed:
        failures.append("no UNLICENSED Solidity file")
    if not saw_muldiv:
        failures.append("EVM emit missing _cam_muldiv helper")
    if not saw_wadd:
        failures.append("EVM emit missing _wadd helper")
    failures.extend(scan_tree(out))
    return failures


def main() -> int:
    root = repo_root()
    fixture = root / "scripts/legal-emit-fixture/legal_emit.cam"
    if not fixture.is_file():
        print(f"FAIL: missing fixture {fixture}", file=sys.stderr)
        return 1
    bin_path = transpiler_bin()
    failures: list[str] = []
    with tempfile.TemporaryDirectory(prefix="cambrian-legal-emit-") as tmp:
        tmp_path = Path(tmp)
        shutil.copy(fixture, tmp_path / "legal_emit.cam")
        lean_out = tmp_path / "out-lean"
        evm_out = tmp_path / "out-evm"
        lean_yaml = tmp_path / "project.lean.yaml"
        evm_yaml = tmp_path / "project.evm.yaml"
        write_yaml(lean_yaml, target="lean", output_dir=lean_out, lean=True)
        write_yaml(evm_yaml, target="evm", output_dir=evm_out, lean=False)
        run_transpile(bin_path, lean_yaml)
        run_transpile(bin_path, evm_yaml)
        failures.extend(check_lean(lean_out))
        failures.extend(check_evm(evm_out))
    if failures:
        print(f"FAIL: {len(failures)} emitted-license problem(s):", file=sys.stderr)
        for f in failures:
            print(f"  - {f}", file=sys.stderr)
        return 1
    print(
        "OK: Lean support files + EVM helpers emitted UNLICENSED, "
        "no GPL identifier in output"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
