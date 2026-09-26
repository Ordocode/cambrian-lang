# T-STD-VAL-002 repro

## Detail

Validator V50 (implicit string→numeric): PRESENT
Control fixture V50: PRESENT (bad control)
Transpile blocked (no silent codegen): yes

V50 diagnostics:
[V50] member 'm_x' transform in route 'go': expression has type `String` but member expects `u64` — use an explicit `std::str::parse_uint` / `std::str::format` call (see docs/STDLIB.md)

Forced EVM codegen:
(forced EVM transpile skipped — validator reject present)

Contract breach (accepted implicit coercion + transpiled): INCONCLUSIVE
