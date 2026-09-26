# Cambrian standard library (`std::`)

> Status: landed. Parent: [LANGUAGE.md](LANGUAGE.md). Lean is an emission
> verifier for the same `.cam` semantics as EVM, not a second dialect.

## 1. Principles

1. **One language** — the same `.cam` source has one semantics. Targets
   (`evm`, `lean`) differ only in **emission**, not in silent dialects.
2. **Strict types** — no implicit string↔number coercion in member transforms, returns,
   or sends. What the types say must match what the expressions produce.
3. **Explicit `std::`** — legacy bare calls such as `min(a, b)` or `pow(x, y)` are
   **rejected by the validator**. Use `std::math::min(a, b)` instead.
4. **Always in scope** — like `msg::` and `sys::`, `std::` needs no `use` import.
5. **Platform intrinsics stay separate** — `gosh::`, `evm::`, `msg::`, `sys::` remain
   platform/context namespaces; they are not part of `std::`.

## 2. Namespace layout (target)

| Namespace | Role | Phase 1 scope |
|-----------|------|----------------|
| `std::math` | Numeric builtins | **yes** — full table below |
| `std::str` | Parse / format text | **yes** — `parse_*` (`Option<T>`), `format` |
| `std::crypto` | Digests | **yes** — `sha256` |
| `std::cell` | StateInit / cell hashes | planned — `stateInit`, `hashOf` |
| `std::address` | Typed address helpers | planned — `addressOf` migration |

Collection helpers (`len`, `map`, `fold`, …) stay **method syntax** on `Vec`, `HashMap`,
and `Option` (`xs.len()`, `xs.map(...)`, …). They are not bare global functions.

## 3. `std::math`

All functions are **pure**. Operand types must match unless the signature documents
widening (same rules as binary `+` / `*` on integers).

| Function | Signature (conceptual) | Semantics |
|----------|------------------------|-----------|
| `std::math::min` | `(T, T) -> T` | Smaller of two values |
| `std::math::max` | `(T, T) -> T` | Larger of two values |
| `std::math::abs` | `(T) -> T` | Absolute value (signed types) |
| `std::math::clamp` | `(T, T, T) -> T` | Clamp `x` to `[lo, hi]` |
| `std::math::muldiv` | `(T, T, T) -> T` | `(x * y) / z` with wide intermediate; panics on `z == 0` |
| `std::math::muldivmod` | `(T, T, T) -> (T, T)` | Quotient and remainder, wide intermediate; panics on zero divisor |
| `std::math::divmod` | `(T, T) -> (T, T)` | `(quotient, remainder)`; panics on zero divisor |
| `std::math::divc` | `(T, T) -> T` | Division rounding up (ceiling); panics on zero divisor |
| `std::math::divr` | `(T, T) -> T` | Division rounding to nearest; panics on zero divisor |
| `std::math::sign` | `(T) -> T` | `-1`, `0`, or `1` for signed types |
| `std::math::minmax` | `(T, T) -> (T, T)` | `(min, max)` pair |
| `std::math::modpow2` | `(T, u64) -> T` | `x % 2^n` (mask semantics) |
| `std::math::pow` | `(T, u64) -> T` | Exponentiation by non-negative integer |

**Numeric types:** any supported integer width (`u8`…`u128`, `i8`…`i128`, `U256`) where
the operation is defined. The validator rejects unsupported combinations at transpile time.

**User `pure fn` names:** a program may define its own `pure fn min(...)`. A bare
`min(...)` then calls the user function, while `std::math::min(...)` always calls the
standard library (on EVM it lowers to a private `_cam_std_min` helper, so the two never
collide). Without a user function of that name, a bare `min(...)` is **V49**.

## 4. `std::str`

### 4.1 Parsing integers (`std::str::parse_*`)

`format` handles **DISPLAY output** (padding, width). `parse` is separate: digits + **radix**,
result width fixed by the **function name** (bit width of the return type).

```cambrian
std::str::parse_u64(s: String, radix: u64) -> Option<u64>
std::str::parse_u8(s, radix) -> Option<u8>
std::str::parse_i64(s, radix) -> Option<i64>
std::str::parse_uint(s, radix) -> Option<u64>   // alias
std::str::parse_int(s, radix) -> Option<i64>    // alias
```

1. Parse all characters in `s` using `radix` (2–36). Leading `0x` / `0X` forces base 16
   (radix must be `16` or `0`).
2. If the numeric value does not fit in the target bit width → `none`.
3. Invalid digit, empty string, bad radix, partial consume → `none`.

There is **no** `parse_display_*` family — string width is not a parse parameter.

**No implicit use:** `String` → numeric member without `std::str::parse_*` → V50 error.

### 4.2 `std::str::format`

```cambrian
std::str::format(fmt: String, ...: Any) -> String
```

Rust-style format string (phase 1):

| Spec | Meaning |
|------|---------|
| `{}` | Default display of argument |
| `{:w}` | Minimum width `w` (decimal digits for integers) |
| `{:0w}` | Zero-pad to width `w` (COBOL DISPLAY-style, e.g. `{:06}` → `000042`) |

Phase 1 supports integers and strings as arguments. Additional types and specifiers
(`{:x}`, precision, etc.) are extensions.

**Examples:**

```cambrian
std::str::format("{}", ws_sum)           // "42"
std::str::format("{:06}", ws_sum)       // "000042"
std::str::format("{},{}", a, b)         // CSV-style join
```

**No implicit formatting:** returning a `u64` from a route with `-> String` without
`std::str::format` is a **validator type error**.

**Removed emission behaviour:** backends must not insert hidden `_cam_atoi` /
`_cam_itoa` (or similar) to paper over type mismatches.

## 5. `std::crypto`

| Function | Signature | Semantics |
|----------|-----------|-----------|
| `std::crypto::sha256` | `(bytes) -> [u8; 32]` | SHA-256 digest |

Bare `sha256(...)` is rejected; use `std::crypto::sha256`.

On EVM, `evm::sha256` remains available for explicit EVM-native call sites; `std::crypto::sha256`
is the cross-target spelling.

## 6. Validator migration

Legacy bare names from the old builtin whitelist (`min`, `max`, `pow`, `sha256`, …)
produce a **hard error** (**V49** — bare stdlib call; use `std::…`), in route bodies and
in `pure fn` bodies alike, unless the program declares a `pure fn` of that name.

Implicit coercion errors (string expression used where numeric type expected without
an explicit `std::str::parse_*` call, or numeric expression assigned to a `String` member
without `std::str::format`) use **V50**.

Unused `let` bindings in route bodies and `pure fn` bodies produce **V51** — reference
the bound name in a later action or expression in the same scope, or remove the `let`.

## 7. Emission requirements

Every `std::` function in the phase-1 tables must lower on **EVM and Lean**,
or the validator must reject the program for that target. No silent
pass-through (e.g. emitting undeclared `pow` in Solidity or
`Cambrian.Unsupported` in Lean).

Verification: conformance tests per `std::math::*` / `std::str::*` cell — see
[TESTING_TARGETS.md](TESTING_TARGETS.md) (forge / lake).

**Generated helpers and license.** EVM injects helpers such as `_cam_muldiv`.
Lean copies `Cambrian/*.lean`. Generated `.sol` / Lean files are not
automatically GPL; they keep
`SPDX-License-Identifier: UNLICENSED`. In-repo prelude assets stay GPL; the
transpiler strips that header on emit. See the root README.

## 8. Conformance harnesses

| Harness | Coverage |
|---------|----------|
| `test_std.rs` | Parser + V49/V50/V51 |
| `test_std_parse.rs` / `test_std_parse_fuzz.rs` | `parse_*` validator + reference fuzz |
| `test_std_evm.rs` | `std_math_matrix` forge |
| `test_std_str_matrix.rs` | `std_str_matrix` — T1 forge + T3 lake |
| `test_std_lean.rs` | `std_lean.cam` codegen + optional lake |

Policy: [TESTING_TARGETS.md](TESTING_TARGETS.md).
