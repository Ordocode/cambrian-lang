# PW3 forge-249 repro (Wave 0 triage)

Baseline: `CAMBRIAN_TEST_COVERAGE_FORGE=1 cargo test -p cambrian-transpiler --test test_audit_coverage_codegen` → **575 PASS / 249 FAIL** @ `ee14c71`.

## Regenerate triage

```bash
CAMBRIAN_TEST_COVERAGE_FORGE=1 cargo test -p cambrian-transpiler \
  --test test_audit_coverage_codegen -- --test-threads=20 2>&1 | tee /tmp/coverage_forge_wave0.log

python3 cambrian-transpiler/tests/audit/repro/pw3_forge_249/triage_wave0.py \
  --log /tmp/coverage_forge_wave0.log
```

## Artifacts

| File | Role |
| --- | --- |
| `triage.csv` | 249 rows — owner sign-off input |
| `triage_summary.md` | lane + validator rollup |
| `triage_validate_manifest.jsonl` | `.cam` snippets for `validate()` |
| `triage_validate_results.jsonl` | validator verdicts |
| `triage_wave0.py` | generator |

Plan: `docs/plans/pw3-coverage-forge-249-closeout.md`.
