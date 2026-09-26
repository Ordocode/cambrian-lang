# PW3-S-009 — full forge gate archive (824/824)

**Closed:** 2026-09-14 @ `e8a6ad6` (`audit`)

## Baseline (start)

`ee14c71` → **575 PASS / 249 FAIL** (824 total). Triage: `pw3_forge_249/`.

## Final gate (verbatim)

```bash
CAMBRIAN_TEST_COVERAGE_FORGE=1 cargo test -p cambrian-transpiler \
  --test test_audit_coverage_codegen -- --test-threads=20
```

**Result @ 2026-09-14:** `824 passed; 0 failed; 0 ignored`

## Meta-gate (opt-in)

```bash
CAMBRIAN_TEST_COVERAGE_FORGE=1 CAMBRIAN_TEST_COVERAGE_FORGE_FULL=1 \
  cargo test -p cambrian-transpiler --test test_audit_phase_n pw3_o009_full_coverage_forge_824 -- --nocapture
```

CI smoke remains the `pw3_o009_*` pilot subset (`test-transpiler-audit-coverage-smoke`).

## Plan

`docs/plans/pw3-coverage-forge-249-closeout.md`
