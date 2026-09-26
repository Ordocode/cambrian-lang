#!/usr/bin/env python3
# Copyright (C) 2025-2026 The Cambrian Authors
# SPDX-License-Identifier: GPL-3.0-only

"""Check a transpiled project's ABI against the surface the standards require.

The Cambrian test harness calls a route directly. It never encodes a call and
never decodes a result, so the *shape* of the external interface is invisible
to it: a route that forgets `-> bool`, spells a parameter `uint256` where the
EIP says `bytes32`, or omits an event entirely will pass every test in the
suite and still be unusable from a Solidity consumer holding the interface.

This closes that blind spot. It reads the compiled artifacts Foundry leaves in
`<project>/build/out/` and compares the real ABI against a declaration of what
the standard demands, written next to the sources as `abi.expected`.

    scripts/abi-check.py stdlib/token
    scripts/abi-check.py stdlib/vault --strict

The declaration lists only what is *required*. Anything else a contract
exposes is reported under --strict and ignored otherwise, because entities
legitimately carry routes beyond the standard (mint, freeze, the generated
member getters).

Declaration syntax, one entry per line:

    <Contract>  <name>(<in>,...) -> (<out>,...)  <mutability>
    <Contract>  event <Name>(indexed <type>, <type>, ...)

`#` starts a comment. A missing `-> (...)` means the function returns nothing.
Mutability is one of pure / view / nonpayable / payable and may be omitted to
skip that part of the check.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from dataclasses import dataclass
from pathlib import Path

MUTABILITIES = {"pure", "view", "nonpayable", "payable"}


@dataclass(frozen=True)
class WantFn:
    contract: str
    name: str
    inputs: tuple[str, ...]
    outputs: tuple[str, ...]
    mutability: str | None

    def signature(self) -> str:
        return f"{self.name}({','.join(self.inputs)})"

    def render(self) -> str:
        out = f" -> ({','.join(self.outputs)})" if self.outputs else ""
        mut = f"  {self.mutability}" if self.mutability else ""
        return f"{self.signature()}{out}{mut}"


@dataclass(frozen=True)
class WantEvent:
    contract: str
    name: str
    inputs: tuple[tuple[str, bool], ...]  # (type, indexed)

    def render(self) -> str:
        parts = [("indexed " if idx else "") + ty for ty, idx in self.inputs]
        return f"event {self.name}({', '.join(parts)})"


class DeclarationError(Exception):
    pass


TYPE_LIST = re.compile(r"\s*,\s*")


def _split_types(raw: str) -> tuple[str, ...]:
    raw = raw.strip()
    if not raw:
        return ()
    return tuple(t.strip() for t in TYPE_LIST.split(raw))


FN_LINE = re.compile(
    r"^(?P<contract>\w+)\s+"
    r"(?P<name>\w+)\((?P<inputs>[^)]*)\)"
    r"(?:\s*->\s*\((?P<outputs>[^)]*)\))?"
    r"(?:\s+(?P<mut>\w+))?\s*$"
)

EVENT_LINE = re.compile(
    r"^(?P<contract>\w+)\s+event\s+(?P<name>\w+)\((?P<inputs>[^)]*)\)\s*$"
)


def parse_declaration(path: Path) -> tuple[list[WantFn], list[WantEvent]]:
    fns: list[WantFn] = []
    events: list[WantEvent] = []

    for lineno, raw in enumerate(path.read_text().splitlines(), start=1):
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue

        if m := EVENT_LINE.match(line):
            inputs = []
            for part in _split_types(m["inputs"]):
                indexed = part.startswith("indexed ")
                inputs.append((part.removeprefix("indexed ").strip(), indexed))
            events.append(
                WantEvent(m["contract"], m["name"], tuple(inputs))
            )
            continue

        if m := FN_LINE.match(line):
            mut = m["mut"]
            if mut is not None and mut not in MUTABILITIES:
                raise DeclarationError(
                    f"{path}:{lineno}: '{mut}' is not a state mutability"
                )
            fns.append(
                WantFn(
                    contract=m["contract"],
                    name=m["name"],
                    inputs=_split_types(m["inputs"]),
                    outputs=_split_types(m["outputs"] or ""),
                    mutability=mut,
                )
            )
            continue

        raise DeclarationError(f"{path}:{lineno}: cannot parse {line!r}")

    return fns, events


def _abi_type(entry: dict) -> str:
    """Canonical ABI type, flattening tuples the way a selector does."""
    ty = entry["type"]
    if ty.startswith("tuple"):
        inner = ",".join(_abi_type(c) for c in entry.get("components", []))
        return f"({inner}){ty.removeprefix('tuple')}"
    return ty


def load_abis(project: Path, build: Path | None = None) -> dict[str, list[dict]]:
    build_root = build or (project / "build")
    out_dir = build_root / "out"
    if not out_dir.is_dir():
        raise SystemExit(
            f"{out_dir} does not exist — transpile and `forge build` the "
            f"project first"
        )

    # Foundry never prunes `out/`, so an artifact can outlive the source that
    # produced it — a scratch probe deleted months ago still answers here, and
    # it answers with the ABI the entity had back then. Only artifacts whose
    # source is still in the tree are evidence.
    src_dir = build_root / "src"

    abis: dict[str, list[dict]] = {}
    for artifact in out_dir.rglob("*.json"):
        if artifact.parent.name == "build-info":
            continue
        if not (src_dir / artifact.parent.name).exists():
            continue
        try:
            data = json.loads(artifact.read_text())
        except (json.JSONDecodeError, UnicodeDecodeError):
            continue
        abi = data.get("abi")
        if not isinstance(abi, list):
            continue
        # A contract can appear under several artifacts (the per-entity file
        # and the bundled project file). They agree; first one wins.
        abis.setdefault(artifact.stem, abi)
    return abis


def actual_functions(abi: list[dict]) -> dict[str, tuple[tuple[str, ...], str]]:
    """signature -> (outputs, mutability)"""
    found = {}
    for e in abi:
        if e.get("type") != "function":
            continue
        ins = tuple(_abi_type(i) for i in e.get("inputs", []))
        outs = tuple(_abi_type(o) for o in e.get("outputs", []))
        found[f"{e['name']}({','.join(ins)})"] = (outs, e["stateMutability"])
    return found


def actual_events(abi: list[dict]) -> dict[str, tuple[tuple[str, bool], ...]]:
    found = {}
    for e in abi:
        if e.get("type") != "event":
            continue
        ins = tuple(
            (_abi_type(i), bool(i.get("indexed"))) for i in e.get("inputs", [])
        )
        found[e["name"]] = ins
    return found


def check(
    project: Path, declaration: Path, strict: bool, build: Path | None = None
) -> tuple[list[str], list[str]]:
    wants_fn, wants_event = parse_declaration(declaration)
    abis = load_abis(project, build)

    failures: list[str] = []
    notes: list[str] = []

    contracts = {w.contract for w in wants_fn} | {w.contract for w in wants_event}
    for contract in sorted(contracts):
        if contract not in abis:
            failures.append(f"{contract}: no compiled artifact carries this name")

    for want in wants_fn:
        abi = abis.get(want.contract)
        if abi is None:
            continue
        have = actual_functions(abi)
        got = have.get(want.signature())
        if got is None:
            failures.append(
                f"{want.contract}.{want.signature()}: absent from the ABI"
            )
            continue
        outs, mut = got
        if outs != want.outputs:
            failures.append(
                f"{want.contract}.{want.signature()}: returns "
                f"({','.join(outs)}), the standard wants "
                f"({','.join(want.outputs)})"
            )
        if want.mutability and mut != want.mutability:
            failures.append(
                f"{want.contract}.{want.signature()}: is {mut}, "
                f"the standard wants {want.mutability}"
            )

    for want in wants_event:
        abi = abis.get(want.contract)
        if abi is None:
            continue
        have = actual_events(abi)
        got = have.get(want.name)
        if got is None:
            failures.append(
                f"{want.contract}: no {want.render()} — the token is not "
                f"indexable without it"
            )
            continue
        if got != want.inputs:
            rendered = ", ".join(
                ("indexed " if idx else "") + ty for ty, idx in got
            )
            failures.append(
                f"{want.contract}.event {want.name}: is ({rendered}), "
                f"the standard wants {want.render()}"
            )

    if strict:
        declared = {(w.contract, w.signature()) for w in wants_fn}
        for contract in sorted(contracts):
            abi = abis.get(contract)
            if abi is None:
                continue
            for sig in sorted(actual_functions(abi)):
                if (contract, sig) not in declared:
                    notes.append(f"{contract}.{sig}: beyond the declaration")

    return failures, notes


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("project", type=Path, help="directory holding project.yaml")
    ap.add_argument(
        "--expect",
        type=Path,
        default=None,
        help="declaration file (default: <project>/abi.expected)",
    )
    ap.add_argument(
        "--build",
        type=Path,
        default=None,
        help=(
            "directory holding out/ and src/ (default: <project>/build) — for "
            "checking a scratch build outside the source tree"
        ),
    )
    ap.add_argument(
        "--strict",
        action="store_true",
        help="also list the surface beyond the declaration",
    )
    args = ap.parse_args()

    declaration = args.expect or (args.project / "abi.expected")
    if not declaration.is_file():
        print(f"no declaration at {declaration}", file=sys.stderr)
        return 2

    try:
        failures, notes = check(args.project, declaration, args.strict, args.build)
    except DeclarationError as exc:
        print(str(exc), file=sys.stderr)
        return 2

    for note in notes:
        print(f"·· {note}")

    if failures:
        for failure in failures:
            print(f"✗  {failure}")
        print(f"\n{len(failures)} ABI conformance failure(s) in {args.project}")
        return 1

    print(f"ABI conforms to {declaration}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
