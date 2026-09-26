# PW3 forge-249 — Wave 0 triage summary

**Date:** 2026-09-11  
**Log:** `/tmp/coverage_forge_cg_syntax1.log`  
**Rows:** 116  
**CSV:** `triage.csv`

## Validator (`validate()` + EVM `check_target_compat`)

| Verdict bucket | Count |
| --- | ---: |
| `OK` | 115 |
| `ERR` | 1 |

**Validator errors (ERR:…):** 1 rows (plan est. ~50)  
**AST-inject (any):** 1 rows · **no harness:** 1  
**Harness output:** 2 rows · **deterministic:** 4 rows  
**Rows with 2+ solc errors:** 25 (plan est. 30)  

### Validator error codes (48 ERR rows)

| Code | Count |
| --- | ---: |
| `E23` | 1 |

## Primary lane distribution

| Lane | Count |
| --- | ---: |
| CG-MISC | 65 |
| CG-EXPR-1 | 17 |
| CG-TYPES-4 | 11 |
| CG-ITER-1 | 5 |
| CG-ROUTE-1 | 5 |
| CG-TYPES-3 | 3 |
| CG-ANALYSIS-1 | 3 |
| CG-TYPES-1 | 2 |
| CG-TYPES-5 | 2 |
| TB-AST | 1 |
| CG-E07 | 1 |
| TB-V | 1 |

## Plan vs triage (heuristic baseline @ plan draft)

| Class | Plan est. | Triage |
| --- | ---: | ---: |
| HF-1 forge-std | 19 | 0 |
| CG-FACTORY det | 21 | 0 |
| TB-V validator | ~50 | 1 ERR / 1 primary lane |
| TB-AST | ~27 | 1 no-harness + 1 primary |
| TB-VEC (`Vec::new()`) | 21 | 0 |
| CG-E07 admitted | ~17 | 1 primary (E07 in validator: 0) |

## Recommended wave order (unchanged from plan)

1. Owner sign-off on this table
2. **TB-V** + **TB-AST** (1 validator ERR + 1 AST-only fixtures)
3. **HF-1** (0 rows) — forge-std harness
4. **CG-FACTORY** (0 rows)
5. Remaining codegen clusters per lane counts above

## DoD

- [x] 249 rows in `triage.csv`
- [x] validator verdict per extractable `.cam`
- [x] solc primary error + lane assignment
- [ ] owner sign-off
