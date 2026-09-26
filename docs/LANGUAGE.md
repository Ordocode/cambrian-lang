# Cambrian Language Reference

Syntax, types, routes, tests, EVM/Lean lowering, and the validation-rule appendix.

## Overview

Cambrian is a stateful, message-based language for smart contract development.
This tree transpiles `.cam` to **Solidity on the EVM** (`--target evm`) and to
a **Lean 4 Lake project** (`--target lean`).

Constructs of other platforms (the `gosh::` namespace, bounce handling,
TVM-only `msg::` / `sys::` fields) are not part of this reference; the
validator rejects or warns about them on EVM and Lean (see the E-rules in
the [Validation Rule Reference](#validation-rule-reference)).

Key principles:
- **Member-centric state** — each state variable owns its transformation logic
- **Temporal operators** — `x` (before) and `^x` (after transformation) for state referencing
- **Pure functions** — side-effect-free computation, separately testable
- **Message-based routing** — incoming messages trigger state transitions and outgoing messages
- **Formal verification ready** — deterministic, no implicit side effects

## Program Structure

A `.cam` file contains top-level declarations in any order:

```cambrian
// Top-level type aliases
type Amount = U256
type TokenId = u64

// Top-level records (shared across entities)
record Pool {
    token_a: address,
    reserve: Amount
}

// Top-level enums (shared across entities)
enum Status { Active, Paused }

// Pure functions (no state access)
pure fn max(a: u64, b: u64) -> u64 {
    if a >= b { a } else { b }
}

// Entity declarations (one or more)
entity MyContract {
    // ... entity body ...
}
```

### Imports

Cambrian has two unrelated `import`-style constructs. Both share a small
surface but serve distinct purposes:

#### Namespace import (`use <ns>`)

The Cambrian **standard library** lives under `std::` (always in scope —
see [STDLIB.md](STDLIB.md)). `evm::` and `msg::` / `sys::` are always in
scope too, so EVM and Lean programs need no `use` line. `use <ns>` exists
for platform namespaces of other targets; an unknown namespace is rejected
(**V21**) and a repeated `use` warns (**W4**).

#### Multi-file `import "./path.cam"` (Phase Library-1)

Cross-file sharing of Cambrian declarations is enabled by a top-level
`import` directive whose argument is a string literal:

```cambrian
// mathlib.cam — a "library file": only declarations, no entities.
pure fn min(a: U256, b: U256) -> U256 { if a < b { a } else { b } }
pure fn max(a: U256, b: U256) -> U256 { if a > b { a } else { b } }

library SafeMath {
    pure fn add(a: U256, b: U256) -> U256 { a + b }
}
```

```cambrian
// Vault.cam — the entity file that consumes the library.
import "./mathlib.cam"

using SafeMath for U256;

entity Vault {
    routes {
        deposit(amount: U256) => []
    }
    m_balance: U256 { in deposit(amount) => m_balance.add(amount) }
}
```

**Semantics:**

- Paths are resolved relative to the importing `.cam` file (not the
  project root). Both forward and backslashes are accepted; the loader
  canonicalises before deduplication.
- Imports are transitive: `A.cam` imports `B.cam` imports `C.cam` ⇒ all
  three are merged into the final `Program`.
- Each file is loaded once (by canonical path), so import cycles are
  allowed: `a.cam` and `b.cam` may import each other and their
  declarations are merged a single time.
- Importable items: `pure fn`, `record`, `enum`, `type`, `const`,
  `event`, `error`, `extern entity`, `library`, `using ... for ...;`,
  and other `import`s.
- **Not** importable: `entity`, `test`, `fuzz`, `invariant`. These remain
  project-owned to prevent silent multi-deployment. **F4** rejects any
  occurrence of an entity/test/fuzz/invariant in a file that's reached
  via `import` or listed under `imports:` in `project.yaml`.
- Duplicate-name conflicts across files reuse the existing
  `merge_programs` HashSet checks (same diagnostic as conflicts inside
  one file). `imports:` does **not** namespace: pulled declarations
  share the program-scope name set (`W9` / duplicate-decl errors stay
  the collision guard).
- A repeated `import` of the same file is deduplicated silently.
- The CLI single-file mode (no `project.yaml`) walks imports starting
  from the entry `.cam`. The `project.yaml`-driven mode still works as
  before — `sources:` lists the *entry points*; transitive imports are
  picked up automatically and need not be enumerated.
- Optional `library_paths:` is a list of search roots (env-expanded
  `${VAR}`, then absolute or yaml-relative). There is **no** built-in
  default root. Optional `imports:` lists library-tier files loaded as
  imported (F4): each spec is tried against the yaml directory first,
  then each root. **F5** fires when a yaml `imports:` entry — or a
  bare (non-`./` / `../`) in-language `import "…"` — hits no root;
  the diagnostic names the paths probed. An explicit `./` / `../`
  miss stays an IO error and does not search `library_paths`. Unset
  `${VAR}` is left literal so it cannot collapse to the project
  directory.

See `examples/uniswap-v2/shared.cam` (extracted helpers, imported by
`UniswapV2Pair.cam`) and `cambrian-transpiler/tests/test_import.rs` for
the full coverage matrix.

## Types

### Primitive Types

| Type | Description | Solidity |
|------|-------------|----------|
| `bool` | Boolean | `bool` |
| `u8`, `u16`, `u32`, `u64`, `u128` | Unsigned integers | `uint8` … `uint128` |
| `i8`, `i16`, `i32`, `i64`, `i128` | Signed integers | `int8` … `int128` |
|   *(EVM)* | Narrow integers survive into Solidity at their declared widths (`uint8`/`uint16`/...) — structs and storage members pack the same way as hand-written Solidity. Integer-widening `uint256` results from arithmetic / `block.timestamp` / `msg::value` are narrowed back via `uintN(...)` casts only when the target binding actually requires the narrower type. ||
| `usize` | Unsigned size | `uint256` |
| `String` | UTF-8 string | `string` |
| `address` | Account address | `address` |
| `bytes` | Byte sequence | `bytes` |
| `U256` | 256-bit unsigned | `uint256` |
| `uint256` | Alias for U256 | `uint256` |
| `pubkey` | 256-bit public key | `bytes32` |

### Generic Types

```text
Vec<u64>                    // Dynamic array
HashMap<address, u64>       // Key-value mapping
Option<u64>                 // Optional value
Address<MyEntity>           // Typed address — required for named message sends
```

### Typed Addresses (`Address<Entity>`)

`Address<Entity>` is a typed wrapper around `address` that carries compile-time information about the target contract type. Named messages can only be sent to typed addresses — sending a named message to a plain `address` is a validation error (V23). For plain value transfers (`~>`), untyped `address` is sufficient.

```cambrian
m_root: Address<RootToken> {}     // typed — can send named messages
m_backup: address {}              // untyped — only plain transfers allowed

macro walletAddr(owner: address) -> Address<TokenWallet> = {
    TokenWallet.address(owner)
}
```

### Tuple Types

```cambrian
(u64, u64, bool)            // Tuple of three elements
```

### Type Aliases

```cambrian
type Amount = U256           // Top-level
type TokenId = u64           // or inside entity
```

## Literals

### Numbers

```text
42                          // Integer
1_000_000                   // Underscore separators (ignored)
0xFF                        // Hexadecimal
0b101010                    // Binary
0b1111_0000                 // Binary with underscores
```

### Strings and Bytes

```text
"hello world"               // String literal
b"raw bytes"                // Bytes literal → vec![0x72, 0x61, ...]
```

### Booleans

```text
true
false
```

### Collections

```text
{}                          // Empty HashMap
array(1, 2, 3)              // Array literal → vec![1, 2, 3]
array()                     // Empty array → vec![]
```

### Optionals

```text
some(42)                    // Some value
none                        // No value
```

On EVM, `Option<T>` is a tagged struct (`Option_uint64`, …), not a
0-sentinel. See [EVM lowering: `Option<T>`](#evm-lowering-optiont).

### Records

```cambrian
Transaction { id: 1, value: 100 }        // Construction
txn { value: new_value }                  // Update (functional)
```

## Expressions

### Arithmetic and Logic

```text
a + b       a - b       a * b       a / b       a % b
a == b      a != b      a < b       a <= b      a > b       a >= b
a && b      a || b      !a
a & b       a | b       a ^ b       a << n      a >> n
```

#### Checked Arithmetic

`+`, `-`, `*` are **checked** — overflow/underflow causes a runtime panic.

```text
a + b       // panics if result overflows the type
a - b       // panics if result underflows
a * b       // panics if result overflows
a / b       // panics (`0x12`) if `b` is zero
a % b       // panics (`0x12`) if `b` is zero
```

Narrowing `as uN` / `as iN` panics (`0x11`) if the value is out of range of the target type. Widening `as` is a no-op (zero- or sign-extend). Wrap-around is `+%` / `-%` / `*%` only.

`std::math::{divc,divr,divmod,muldiv}` also panic on a zero divisor on every target (independent of `lean.numerics`). Overflow-wrap only covers `+%`/`-%`/`*%`.

#### Wrapping (Unchecked) Arithmetic

`+%`, `-%`, `*%` are **wrapping** — overflow wraps around (modular arithmetic).

```text
a +% b      // wrapping add: u8::MAX +% 1 == 0
a -% b      // wrapping sub: 0u8 -% 1 == 255
a *% b      // wrapping mul: u8::MAX *% 2 == 254
```

On the **EVM target** these lower to per-contract `internal pure`
helpers (`_wadd` / `_wsub` / `_wmul`) wrapped in `unchecked { … }`,
preserving Solidity-0.8 wrap-around semantics without disabling
Cambrian's checked arithmetic elsewhere in the contract. The helpers
are emitted only when the contract uses any wrapping op. The cumulative
price oracle in `examples/uniswap-v2/UniswapV2Pair.cam` is the
canonical use case (`UQ112x112` math intentionally overflows on the
112-bit boundary).

#### Lean numerics (`lean.numerics`)

Source-language `+`/`-`/`*` stay **checked** (above). How the **Lean**
model interprets those operators is selected in `project.yaml`:

| `lean.numerics` | Integer carrier | `+` `-` `*` | `+%` `-%` `*%` |
|-----------------|-----------------|-------------|----------------|
| *(absent)* or `overflow-wrap` | `BitVec n` (`u*` and `i*`) | wrapping infix (default); signed `<`/`/`/`>>` use `slt`/`sdiv`/`sshiftRight` | wrapping infix |
| `overflow-panic` | `BitVec n` | unsigned → `Cambrian.checkedAdd`/`checkedSub`/`checkedMul`; signed → `checkedSAdd`/`checkedSSub`/`checkedSMul` → `RouteResult` fail (`ThrowCode` `0x11`) | wrapping infix |
| `nat` | `Nat` (`u*`/`U256`/`usize`) / `Int` (`i*`) | total/saturating `Nat`/`Int` (INTENDED proof model; overflow ignored) | degrades to `Nat`/`Int` `+` |

```yaml
lean:
  numerics: overflow-panic   # or overflow-wrap (default) or nat
```

`nat` is a **simplified proof model** of real (EVM) computation: it
exists to make Lean proofs easier, not to reproduce Solidity 0.8
panics. Unsigned `-` is Lean's total saturating `Nat` subtraction
(`0 - 1 = 0`); overflow on `+`/`*` is ignored; `/ 0` is Lean's total
`Nat` division (not Panic 0x12). Signed values inhabit Lean `Int`.
That divergence from EVM is **INTENDED** (audit PW3-O-002). Use
`overflow-panic` when the Lean model must match checked arithmetic
(signed overflow uses two's-complement `Int` range checks).
Unknown values are rejected (`F2`).

#### Automatic Type Widening

When operands have different numeric types, the compiler automatically casts
both to the smallest common type (no explicit casts needed):

**Rule 1 — Same sign:** `max(width)`, keep sign.
```text
m_total: u128 + amount: u64    // amount widened to u128
m_x: i32 + m_y: i64            // m_x widened to i64
```

**Rule 2 — Mixed sign:** signed type with `width = max(2 × unsigned_width, signed_width)`.
```text
m_count: u32 + delta: i8       // both widened to i64
m_val: u64 + offset: i64       // both widened to i128
```

**Rule 3 — u128/U256 + signed:** compile error (no primitive type covers both ranges).

**U256:** any unsigned type auto-widens to U256 when combined with a U256 operand.
Integer literals adapt automatically: `m_balance + 1` where `m_balance: U256` casts `1` to `U256`.

Full widening table:

| | u8 | u16 | u32 | u64 | u128 | U256 | i8 | i16 | i32 | i64 | i128 |
|---|---|---|---|---|---|---|---|---|---|---|---|
| **u8** | u8 | u16 | u32 | u64 | u128 | U256 | i16 | i16 | i32 | i64 | i128 |
| **u16** | u16 | u16 | u32 | u64 | u128 | U256 | i32 | i32 | i32 | i64 | i128 |
| **u32** | — | — | u32 | u64 | u128 | U256 | i64 | i64 | i64 | i64 | i128 |
| **u64** | — | — | — | u64 | u128 | U256 | i128 | i128 | i128 | i128 | i128 |
| **u128** | — | — | — | — | u128 | U256 | ERR | ERR | ERR | ERR | ERR |
| **U256** | — | — | — | — | — | U256 | ERR | ERR | ERR | ERR | ERR |
| **i8–i128** | (symmetric) | | | | | | i8 | i16 | i32 | i64 | i128 |

### Field Access and Indexing

```text
record.field                // Field access
map[key]                    // Index access (missing key → type default, e.g. 0)
expr as Type                // Type cast (narrowing `as uN` panics 0x11 if out of range)
```

A missing `HashMap` key does **not** revert. `m[k]` (and nested `m[k1][k2]`)
returns the value type's default — `0` for numeric maps — the same as a
Solidity `mapping`. Stored `0` and an absent key are indistinguishable through
`[]`. Use `m.exists(k)` / `m.contains(k)` when membership matters. (PW3-G-006
**WONT FIX**; intended for now.)

### Method Calls

```text
list.len()
map.exists(key)
map.contains(key)            // alias of exists(); preferred name on EVM
map.is_empty()               // true iff len() == 0
items.iter().filter(|x| x > 0).collect()
```

`HashMap` exposes a small membership / cardinality surface that lowers
to native primitives on every backend:

| Method            | EVM (Solidity)                                                          | Lean (P4c+)                                                          |
|-------------------|-------------------------------------------------------------------------|------------------------------------------------------------------------|
| `m.exists(k)`     | `m_exists[k]` (companion bool mapping kept in sync by the writers).     | `Cambrian.AddressMap.contains` / `lookup` + `Option.isSome`.           |
| `m.contains(k)`   | `m_exists[k]` (same lowering as `exists`).                              | same as `exists`.                                                      |
| `m.is_empty()`    | `m_keys.length == 0` (iterable companion array, see `for (k, v) in m`). | `AddressMap.isEmpty` (or ghost `{m}_keys` when iterated).              |
| `m.len()`         | `m_keys.length`.                                                        | `AddressMap.length` (pair count; ghost `{m}_keys.length` when iterated). |


### Control Flow

```text
if condition { then_expr } else { else_expr }

match m_status {
    Status::Active => 1,
    Status::Paused => 0,
    _ => 2
}

let x = compute();
x + 1
```

### Iteration (`for`)

`for x in iter { body }` is a value-producing expression — semantically
equivalent to `iter.map(|x| body).collect::<Vec<_>>()`. The iterator
can be a half-open range (`start..end`) or any expression that resolves
to a `Vec<T>`.

```cambrian
pure fn double_all(xs: Vec<u32>) -> Vec<u32> {
    for x in xs { x * 2 }
}

pure fn first_n(n: u32) -> Vec<u32> {
    for i in 0..n { i }
}
```

**Cross-target support:**

| Form                                                  | EVM | Lean (P4c+) |
| ----------------------------------------------------- | :-: | :---------: |
| `for x in start..end { body }`                        |  ✓  |      ✓      |
| `for x in vec_expr { body }`                          |  ✓  |      ✓      |
| `(start..end).fold(init, \|acc, x\| body)`            |  ✓  |      ✓      |
| `vec_expr.fold(init, \|acc, x\| body)`                |  ✓  |      ✓      |
| `xs.iter() / .map() / .filter() / .take()` chains     |  ✓  |      ✓      |
| `xs.enumerate()` and `for (i, x) in xs.enumerate()`   |  ✓  |      ✓      |
| `for (k, v) in m.iter()` / `for (k, v) in m`          |  ✓  |      ✓      |
| `m.values().collect()` / `m.keys().collect()`         |  ✓  |      ✓      |
| `.fold` with record/tuple accumulator                 |  ✓  |      ✓      |
| `\|x\| body` (closure as a value, outside `.fold`)    |  ✗  |      ✗      |

On EVM, all supported `for` and chain shapes fuse into a single
Solidity `for`-loop. `.fold` lowers to a sibling for-loop that
threads its accumulator across iterations — scalar, tuple, and
record accumulators are all supported (the only rejected shape is
an in-memory `HashMap` accumulator, since Solidity has no in-memory
mappings; use a storage map instead). Patterns like the canonical
Babylonian sqrt below lower directly:

```cambrian
pure fn babylonian_sqrt(x: U256) -> U256 {
    (0..7).fold(
        x,
        |r, _i| if r == 0 { 0 } else { (r + x / r) / 2 }
    )
}
```

See `contracts/fold_evm.cam` and `contracts/loop_evm.cam` for fixtures; iterator
chain fusion is covered by `evm12_*` tests in `test_codegen_evm.rs`.

#### Action-level `for` (effectful loops)

Inside a route body, `for <pat> in <iter> => [ <actions> ]` runs the
action list once per element. Unlike the expression-level `for`,
this form does **not** produce a value — it sequences effects (sends,
deploys, plain transfers, state writes, lets) per iteration.

```cambrian
entity Airdrop {
    routes {
        airdrop(amount: uint256, recipients: Vec<address>) => [
            for r in recipients => [
                ~> r with { value: amount }
            ]
        ]
    }
}
```

EVM lowers this to a Solidity `for` loop and Lean to a fold over the
iterated list. Iterator sources may be:

- a route parameter `Vec<T>`,
- a state member `Vec<T>` or `HashMap<K, V>` (with `for (k, v) in m`
  sugar that walks the map's parallel keys array),
- a half-open range `start..end`.

**Loop body restrictions (V29).** To preserve the
checks-effects-interactions invariant on EVM, the loop body cannot contain:

- a `var x = msg(args) ~> dest` capture (synchronous return capture
  cannot be carried across iterations safely),
- `return(...)` (a route returns at most once).

If you need any of these, do the loop-free part outside the loop and
keep the loop body as effects-only.

### Blocks

Blocks can appear in `let` bindings, as final expressions, or nested within other blocks:

```cambrian
{
    let a = f(x);
    let b = g(a);
    a + b
}
```

Nested blocks:

```cambrian
let result = {
    let intermediate = {
        let step = x * 2;
        step + 1
    };
    intermediate + 10
};
```

## Entity Declaration

An entity is the core unit — a stateful contract with routes (message handlers)
and members (state variables).

```cambrian
entity Token {
    // Inner type aliases
    type Amount = U256

    // Records (entity-scoped)
    record Allowance {
        spender: address,
        amount: Amount
    }

    // Enums
    enum Status { Active, Paused }

    // Constants
    const MAX_SUPPLY: Amount = 1_000_000
    const DECIMALS: u8 = 18

    // Macros (state-aware helper functions)
    macro remaining() -> Amount = {
        MAX_SUPPLY - m_supply
    }

    // Routes (message handlers)
    routes {
        #[factory_only]
        constructor() => []
        mint(to: address, amount: Amount)
        where amount <= @remaining() : throw 1
        => []
    }

    // Members (state variables with transformations)
    m_supply: Amount {
        in constructor() => 0
        in mint(_, amount) => m_supply + amount
    }
}
```

## Routes

Routes are message handlers inside a `routes { }` block. Each route can have
`from` (sender verification), `where` (preconditions), a return type, and a body.

### Regular Route

```cambrian
transfer(to: address, amount: U256)
from Owner(m_owner_key)
where amount > 0 : throw 100
&& m_balance >= amount : throw 101
=> [
    Receipt(to, amount) ~> msg::sender
]
```

### View Route

Cannot modify state (no member transforms allowed). Used for read-only queries.

```cambrian
view getBalance() -> U256 => [
    return(m_balance)
]
```

A value-bearing `return(expr)` requires a declared `-> T` on the route
(**V42**). A bare `return` / `return()` (no value) is a valid early exit
and does not need a return type. Without `-> T`, the EVM backend would
emit `return expr;` inside a function with no `returns(...)` clause
(solc 8863).

### Pure Route

Cannot access state, msg:: context, macros, or temporal refs. Result depends only
on arguments — fully deterministic.

```cambrian
pure add(a: u64, b: u64) -> u64 => [
    return(a + b)
]
```

### Init Route

Special one-time initialization route. Each entity can have at most one `init` route.
It creates a default state and initializes it. Cannot have `from` clauses or a return type.

```cambrian
init setup(owner: address, supply: U256)
where supply > 0 : throw 1
=> [
    // member transforms set initial values
]
```

### Private Route

A `private` route is not part of the contract's external interface. Other
routes of the same entity invoke it with the `call` action:

```cambrian
entity Vault {
    routes {
        deposit(amount: U256) => [
            call credit(amount)
        ]
        private credit(amount: U256) => []
    }
    m_balance: U256 { in credit(amount) => m_balance + amount }
}
```

`call` is an in-process invocation: the callee runs against the same
instance and the same message context, so `msg::sender` inside the callee
is the original caller. On EVM a private route lowers to an `internal`
Solidity function `_name` (not part of the ABI) and `call` lowers to a
direct internal call; on Lean the callee receives the caller's `ctx`. A
`view` route may only `call` routes that have no member transforms and no
state-changing actions, transitively (**V9**).

### Extern Entity Declarations

`extern entity` declarations describe the surface of a *foreign*
contract — one that lives outside the current Cambrian project but
that this contract needs to call. They are interface stubs: there is
no body, no member section, no transforms, only route signatures.

```cambrian
extern entity Token {
    route transfer(amount: U256);
    view route balanceOf(who: address) -> U256;
    accept route deposit() -> bool;
}

entity Caller {
    routes {
        #[factory_only]
        constructor(t: Address<Token>) => []
        ping(amount: U256) => [
            transfer(amount) ~> m_token        // typed send
        ]
        view checkBalance(who: address) -> U256 => [
            var b = balanceOf(who) ~> m_token; // var-call captures return
            return(b)
        ]
    }
    m_token: Address<Token> {
        in constructor(t) => t
    }
}
```

| Modifier        | Solidity mapping              | Notes                                                                      |
|-----------------|-------------------------------|----------------------------------------------------------------------------|
| (none)          | non-payable, non-view         | Default mutating route.                                                    |
| `view route`    | `view`                        | Read-only; valid in EVM `staticcall` paths.                                |
| `accept route`  | `payable`                     | Allows attached `value:` on the call site (mirrors entity-route semantics).|

Use it whenever:

- The target contract is built separately (audited library, third-party
  ERC-20, an ABI you reference but don't own).
- You want typed `Address<Token>` members and var-calls (`var x =
  msg(args) ~> dest`) without including the implementation in the
  build graph.

**Codegen behaviour:**

- **EVM:** the transpiler emits a populated Solidity
  `interface I<Name> { ... }` declaration with the listed routes (and
  the right `view` / `payable` modifiers). The interface is referenced
  by every `~> dest` and `var ... ~> dest` whose target type is
  `Address<Name>`.
- **Lean:** each extern route becomes an opaque axiom
  `(w, ctx, args…) : World × T` in `Cambrian.Generated.Extern`.

**Validator contract:**

- **V30** — duplicate `extern entity` name within the program, or
  collision with a real `entity` of the same name.
- **V31** — duplicate route signature inside the same `extern entity`
  block.
- **V23** — typed sends still resolve their target route; an extern
  route lookup that misses fires V23.
- **V32** — `deploy ExternEntity(...)` arity is intentionally **not**
  enforced (the constructor surface is unknown).
- **E22** *(EVM target)* — a typed send whose destination resolves to
  an entity name that's neither in `program.entities` nor declared as
  `extern entity` is rejected at validation time.

Since `extern entity` is a pure surface declaration, the routes inside
it inherit the same `from` / `where` / `accept` parsing rules as
regular routes but never carry a body.

### Route Body Actions

```cambrian
settle(amount: U256) => [
    // Send a message to a destination
    Transfer(amount) ~> m_target

    // Send with options (attached value)
    PaymentReleased(amount) ~> m_seller with { value: amount }

    // Plain value transfer (no function call, just send ETH)
    ~> m_recipient with { value: amount }

    // Conditional actions
    if amount > 1000 => [
        Notify(amount) ~> admin
    ] else [
        Ack() ~> msg::sender
    ]

    // Let bindings
    let fee = compute_fee(amount);
    TransferWithFee(amount, fee) ~> target

    // Call a private route
    call credit(amount)

    // Return value (for view/pure routes)
    return(m_balance)
]
```

#### `return` / `throw` control flow (EVM + Lean)

`return(...)` and `throw` / `throw Name(...)` are **terminators**: later
actions in the same route (or phase) body do not run. On EVM this matches
Solidity `return` / `revert`. On Lean, world-threaded and fail-mode route
bodies short-circuit the same way (no dead `emit` / send after a
terminator). This is intentional alignment: **EVM is ground truth** for
the Lean model.

#### Send Options (`with`)

The `with { ... }` block after `~>` provides message parameters. On EVM
and Lean the supported option is `value`.

```cambrian
// Without options (uses platform defaults)
Transfer(amount) ~> dest

// With explicit options
Transfer(amount) ~> dest with { value: amount }

// Plain value transfer (no message body, no function call)
~> dest with { value: amount }

// Options available depend on the target:
// - EVM: value (native ETH attached to the call / transfer)
// - Lean: value (must match EVM ledger + msg::value semantics)
```

On **EVM**, `with { value: n }` attaches `n` wei to the CALL / CREATE and
debits the sender balance. On **Lean**, the same option:

- for a **typed send** / `var` capture, threads `n` into the callee
  `MsgCtx` via `Cambrian.MsgCtx.withValue` (so `msg::value` in the callee
  matches Forge);
- for a **dynamic** send (`~> expr` where `expr` is not a static
  `Entity.address(...)`), emits `Cambrian.WorldState.transfer` before the
  dispatch axiom so balances move like on-chain ETH.

### From Clause (Sender Verification)

Checks that `msg::sender` matches a computed entity address.
Multiple sources can be joined with `|` (OR).

```cambrian
// Single sender
withdraw() from Owner(m_owner_id) => []

// Multiple allowed senders
pause() from Owner(m_owner_id) | Admin(m_admin_id) => []
```

There is no contained-failure (`try` / `catch`) mechanism: a failing
`where`, `from` or `throw` aborts the whole route.

### Where Clause (Preconditions)

Guards with error codes. Multiple conditions are chained with `&&`.

```cambrian
take(amount: U256)
where amount > 0 : throw 100
&& m_balance >= amount : throw 101
=> []
```

The `: throw N` form lowers to a numeric `revert("throw(N)")` on EVM. To use a typed Solidity
custom error instead, declare an `error` (see [Custom Errors](#custom-errors-evm))
and write `: throw ErrorName(args)`.

## Events (EVM)

`event` declarations describe Solidity log topics. They can appear at
program scope (file scope) or inside an entity's `routes { }` block,
and are emitted with the `emit` action.

```cambrian
entity ERC20 {
    event Transfer(indexed src: address, indexed dst: address, amount: U256);
    event Approval(indexed owner: address, indexed spender: address, amount: U256);

    routes {
        transfer(dst: address, amount: U256) => [
            emit Transfer(msg::sender, dst, amount);
        ]
    }
}
```

**Rules:**

- A parameter may be marked `indexed` — at most three indexed
  parameters per non-anonymous event (EVM ABI topic limit, enforced by
  **V36** on EVM-domain targets only; other domains ignore `indexed`).
- `emit Name(args)` arity / argument types must match the declaration
  (**V35**); narrow integers are auto-cast to the declared widths.
- `emit` of an undeclared event is rejected by **V34**.
- Pure routes cannot `emit` (V11).

**Target lowering:**

- **EVM:** declarations lower to Solidity `event Name(...);`; `emit`
  lowers to `emit Name(args);`. Program-scope events are emitted at
  file scope.

## Custom Errors (EVM)

`error` declarations describe Solidity custom-error ABIs. They can
appear at program or entity scope and are raised either through a
`throw Name(args)` action or from a `where` / `from` clause via
`: throw Name(args)`.

```cambrian
error InsufficientBalance(have: U256, need: U256);

entity Owner {
    identity m_id: u64

    routes {
        #[factory_only]
        constructor() => []
    }
}

entity Vault {
    identity m_owner: u64

    error Unauthorized();

    routes {
        #[factory_only]
        constructor() => []

        withdraw(amount: U256)
        from Owner(m_owner)
        where m_balance >= amount : throw InsufficientBalance(m_balance, amount)
        => [
            ~> msg::sender with { value: amount }
        ]

        kill()
        from Owner(m_owner) : throw Unauthorized()
        => [
            throw Unauthorized()
        ]
    }

    m_balance: U256 {
        in constructor() => 0
        in withdraw(amount) => m_balance - amount
    }
}
```

**Rules:**

- `throw Name(args)` references must match an `error` declaration
  visible at program or entity scope (**V38**).
- Argument arity / types must match the declaration (**V39**); narrow
  integers are auto-cast.
- The legacy numeric form `throw N` and `: throw N` is unchanged and
  continues to work (lowers to `revert("throw(N)")`).

**Target lowering:**

- **EVM:** declarations lower to Solidity `error Name(...);`;
  `throw Name(args)` lowers to `revert Name(args);`. Program-scope
  errors are emitted at file scope.

## EVM `receive` / `fallback` Routes

The route names `receive` and `fallback` are reserved on the EVM
target and lower to Solidity's special functions of the same name.

```cambrian
entity EthVault {
    routes {
        accept receive() => [
            // any Cambrian effects / member transforms
        ]

        fallback() => [
            // optional, runs on calls with no matching selector
        ]
    }
    // ...
}
```

**Rules:**

- Both routes must take no parameters and have no return type
  (`view` / `pure` are also rejected) — enforced by **V40**.
- `receive` must carry the `accept` modifier (V40); the generated
  `receive() external payable` is always `payable`.
- `fallback()` is auto-`payable` if (and only if) the body reads
  `msg::value`.
- An entity may declare at most one of each (**V41**).

## Members (State Variables)

Members define state and how it transforms when routes are triggered.

```cambrian
m_balance: U256 {
    in constructor(initial) => initial
    in deposit(amount) => m_balance + amount
    in withdraw(amount) => m_balance - amount
}

// Member with no transforms (set externally or remains default)
m_metadata: String {}
```

### Identity Members

Identity members define the fields that determine the contract's
deterministic (CREATE2) address. They are immutable — no transforms or
default values are allowed. Constructor parameters are ordinary deploy
arguments and do not affect the address.

```cambrian
entity Vault {
    identity m_id: u64          // affects address, set at deploy

    routes {
        #[factory_only]
        constructor(initial_balance: u64) => []  // regular arg, not static
    }

    m_balance: u64 {
        in constructor(initial_balance) => initial_balance
    }
}
```

An entity without identity members is a singleton: it deploys at a fixed
address and `Entity.address()` takes no arguments.

### Temporal References

Inside a member transform, `^member_name` refers to the **already-computed**
value of another member in the same route execution. The transpiler ensures
correct computation order via topological sorting. Using `^member` outside a
transform body (`where`, `from`, route actions, defaults) is a **V43** error.

```cambrian
m_count: u64 {
    in addItem() => m_count + 1
}

m_is_full: bool {
    in addItem() => ^m_count >= MAX_ITEMS
    //              ^^^^^^^^ uses the NEW value of m_count
}
```

### Transform Parameters

Transform parameter patterns mirror route parameters but can use `_` for ignored args.

```cambrian
m_balance: U256 {
    in transfer(to, amount) => m_balance - amount
    in transferFrom(sender_addr, _, amount) => m_balance + amount
}
```

### Appending to `Vec<T>` Members

A transform whose final expression is `<member>.push(arg)` against a
`Vec<T>`-typed member lowers as a statement-level append rather than a
recomputation of the whole vector. The body may freely introduce
`let` bindings before the `.push(...)`:

```cambrian
m_all_pairs: Vec<address> {
    in createPair(tokenA, tokenB) => {
        let pair = UniswapV2Pair.address(min_addr(tokenA, tokenB),
                                         max_addr(tokenA, tokenB));
        m_all_pairs.push(pair)
    }
}
```

On EVM this lowers to a literal `m_all_pairs.push(pair);` against the
`address[]` storage array; on Lean it appends to the member's list.
`.push(...)` is **only** legal as
the tail of a `Vec<T>` member transform — it is rejected anywhere else
(including inside pure functions or `let` initialisers).

### Per-Transform `let` Scoping

Each transform body that introduces a `let` (or otherwise needs its
own scope) is emitted inside its own `{ … }` block on EVM. Sibling
transforms for the same route can therefore reuse the same `let` name
without colliding:

```cambrian
m_get_pair: HashMap<U256, address> {
    in createPair(tokenA, tokenB) => {
        let pair = UniswapV2Pair.address(min_addr(tokenA, tokenB),
                                         max_addr(tokenA, tokenB));
        m_get_pair
            .update(hashOf(tokenA, tokenB), pair)
            .update(hashOf(tokenB, tokenA), pair)
    }
}

m_all_pairs: Vec<address> {
    in createPair(tokenA, tokenB) => {
        let pair = UniswapV2Pair.address(min_addr(tokenA, tokenB),
                                         max_addr(tokenA, tokenB));
        m_all_pairs.push(pair)
    }
}
```

## Pure Functions

Declared at top level, outside entities. Cannot access state, msg::, macros, or temporal refs.

```cambrian
pure fn check_bit(mask: u32, index: u8) -> bool {
    (mask >> index) & 1 == 1
}

pure fn clamp(value: u64, min_val: u64, max_val: u64) -> u64 {
    if value < min_val { min_val } else { if value > max_val { max_val } else { value } }
}
```

## Libraries and `using` (Phase Library-2 / Library-3)

Cambrian offers two complementary mechanisms for organising and reusing
`pure fn` helpers.

### `using ... for T` — method-call sugar

```cambrian
pure fn double(x: U256) -> U256 { x * 2 }
pure fn triple(x: U256) -> U256 { x * 3 }

using { double, triple } for U256;

entity Foo {
    routes {
        bump() => []
    }
    m_n: U256 { in bump() => m_n.double().triple() }
}
```

`recv.fn(args)` rewrites to `fn(recv, args)` at AST-resolution time
(pre-codegen pass, [src/using_rewrite.rs](../cambrian-transpiler/src/using_rewrite.rs)).
The rewrite is pure sugar — no runtime cost, and the EVM and Lean
backends see the same desugared call. Chained calls (`x.double().triple()`)
work because the rewriter inspects each `pure fn`'s declared return type.

**Validators:**

- **V55** — `using` method-name collides with a built-in for the
  receiver type (`Vec` / `HashMap` / `String` / `Address`).
- **V56** — `using { fn1 } for T;` or `using LibName for T;` references
  an undeclared `pure fn` / `library`.
- **V57** — referenced `pure fn`'s first parameter type does not match
  the `for` type.

### `library Name { ... }` keyword

A named scope of `pure fn` + `const` + `type` items, used either through
direct qualified calls or via `using LibName for T;` sugar:

```cambrian
library SafeMath {
    const LIMIT: U256 = 1_000_000
    pure fn add(a: U256, b: U256) -> U256 { a + b }
    pure fn sub(a: U256, b: U256) -> U256 { a - b }
}

using SafeMath for U256;

entity Vault {
    routes {
        deposit(amount: U256) => []
    }
    m_balance: U256 { in deposit(amount) => m_balance.add(amount) }
    m_cap: U256 { in deposit(amount) => SafeMath.LIMIT }
}
```

Library functions are called as `x.add(y)` (through `using`),
`SafeMath.add(x, y)` or `SafeMath::add(x, y)`; constants as
`SafeMath.LIMIT` or `SafeMath::LIMIT`. Inside a library body, sibling
functions and constants are referenced unqualified. `+` / `-` / `*` are
already checked, so overflow panics without an explicit precondition.

```cambrian
library Wad {
    const ONE: U256 = 1_000_000_000_000_000_000
    pure fn mul(a: U256, b: U256) -> U256 { a * b / ONE }
    pure fn sq(a: U256) -> U256 { mul(a, a) }
}
```

**Body items:** only `pure fn`, `const`, and `type` aliases. Library
bodies may not access state, `msg::*`, or temporal references — this is
the same purity envelope as a free `pure fn`.

**EVM lowering:** one Solidity `library Name { ... }` block per Cambrian
`library`, with each `pure fn` lowered to an `internal pure` (or
`internal view`, when the body reads no state) Solidity function.
Call sites use the `LibName.fn(args)` qualifier:

```solidity
library SafeMath {
    function add(uint256 a, uint256 b) internal pure returns (uint256) { ... }
    function sub(uint256 a, uint256 b) internal pure returns (uint256) { ... }
}

contract Vault {
    function deposit(uint256 amount) external {
        m_balance = SafeMath.add(m_balance, amount);
    }
}
```

Free `pure fn`s (declared outside any `library`) continue to lower to
top-level Solidity free functions, so existing fixtures are unchanged.

**Lean lowering:** library functions and constants become definitions
`Cambrian.Generated.Pure.<Lib>_<name>` (a constant is a zero-argument
definition); every call form above resolves to them.

**Validators:**

- **V47** — duplicate item name inside a `library`.
- Items other than `pure fn` / `const` / `type` are a parse error.

See `cambrian-transpiler/tests/test_library.rs` and
`cambrian-transpiler/tests/test_using.rs` for the full coverage matrix
(parser, rewrite, codegen, solc round-trip).

## Macros

Entity-scoped helper functions that CAN access state. Referenced with `@name(args)`.

```cambrian
macro is_owner() -> bool = {
    msg::sender == m_owner
}

// Usage in where clause:
routes {
    admin_action()
    where @is_owner() : throw 403
    => []
}
```

## Records

Named product types with fields. Can be declared at top level or inside entities.

```cambrian
record Transaction {
    id: u64,
    sender: address,
    value: U256,
    confirmed: bool
}
```

**Construction:** `Transaction { id: 1, sender: addr, value: 100, confirmed: false }`

**Functional update:** `txn { confirmed: true }` (creates new record with one field changed)

## Enums

Sum types. Can be declared at top level or inside entities.

### Unit Variants

```cambrian
enum State { Created, Active, Completed, Cancelled }
```

### Variants with Data

```cambrian
enum Message {
    Transfer(u64),
    Approve,
    SetOwner(address)
}
```

### Match Expressions

```cambrian
pure fn state_code(s: State) -> u64 {
    match s {
        State::Active => 1,
        State::Created => 0,
        _ => 2
    }
}

// Destructuring data variants
pure fn transfer_amount(m: Message) -> u64 {
    match m {
        Message::Transfer(amount) => amount,
        Message::SetOwner(_) => 0,
        _ => 0
    }
}
```

A `match` must cover every variant or end with `_` (**V66**); arms after
`_` are unreachable (**V47**).

### EVM lowering: tagged-union enums

Solidity has no native sum type, so payload-bearing enums lower to a
**tagged-union struct** on the EVM target:

```solidity
// enum Action { Deposit(U256), Approve, SetOwner(address) }
enum Action_Tag { Deposit, Approve, SetOwner }
struct Action {
    Action_Tag tag;
    uint256 deposit_0;     // payload of Deposit at position 0
    address  setowner_0;   // payload of SetOwner at position 0
}
```

Field-naming convention:

- One **tag enum** `<Name>_Tag` with one variant per Cambrian variant
  (preserves source order; matches the discriminant assignment
  `match m_state { Tag::A => 0, Tag::B => 1, ... }`).
- One **struct** `<Name>` with a `tag` field plus
  `<lowercase_variant>_<positional_index>` fields, one per payload
  position across **all** payload-bearing variants of the enum (every
  `Action` value reserves slots for `Deposit`'s payload *and*
  `SetOwner`'s payload — the wasted slots default to zero values).

Constructors lower to struct literals with type-aware defaults:

```solidity
// Cambrian:  Action::Deposit(amount)
Action({ tag: Action_Tag.Deposit, deposit_0: amount, setowner_0: address(0) })

// Cambrian:  Action::Approve
Action({ tag: Action_Tag.Approve, deposit_0: 0, setowner_0: address(0) })
```

Matches dispatch on the `.tag` field; per-arm bindings read from the
matching payload field:

```solidity
if (action.tag == Action_Tag.Deposit) {
    // amount in Cambrian == action.deposit_0
} else if (action.tag == Action_Tag.SetOwner) {
    // addr   in Cambrian == action.setowner_0
}
```

Trade-offs:

- **Storage / calldata cost.** Every `Action` value carries a slot for
  *every* payload position of the entire enum. For two-variant payload
  enums this is fine; for very large enums prefer a discriminator
  union pattern instead.
- **Default initialisation.** The unused slots get type-aware defaults
  (`0` / `address(0)` / `false` / empty arrays), which means an
  uninitialised `Action` lands on the first-declared variant with
  zeroed payload — keep this in mind when designing the variant order.

The lowering is shipped as Phase EVM-4 J1-J3. See [`docs/EVM_GAPS.md`
§ 1.31](EVM_GAPS.md) for the design rationale, and
`contracts/enum_data.cam` for the canonical fixture. Validation
emits the informational warning **E08** at every payload-bearing
variant construction so users see the storage-cost trade-off without
the call being rejected.

### EVM lowering: `Option<T>`

`Option<T>` is **not** erased to `T`. It lowers to a tagged-union
struct, the same shape as payload-bearing enums, so `some(0)` is
distinct from `none` (and `some(address(0))` is distinct from
`none`):

```solidity
// Option<u64>
enum Option_uint64_Tag { None, Some }
struct Option_uint64 {
    Option_uint64_Tag tag;
    uint64 some_0;
}
```

Constructors and match:

```solidity
// Cambrian: none
Option_uint64({ tag: Option_uint64_Tag.None, some_0: 0 })

// Cambrian: some(v)
Option_uint64({ tag: Option_uint64_Tag.Some, some_0: v })

// Cambrian: match m_t { none => 111, some(_) => 222 }
(m_t.tag == Option_uint64_Tag.None ? 111 : 222)
```

`some(x)` binders read `m_t.some_0`. Nested options compose
(`Option<Option<u64>>` → `Option_Option_uint64` whose payload is
`Option_uint64`). Storage members of option type occupy their own
slot(s) and do not pack with a following scalar.

See `contracts/` fixtures and PW3-O-001 (`tests/audit/fixtures/x_pw3_option_zero.cam`).

## Message Context (`msg::`)

Available inside route bodies and member transforms (but NOT in pure functions or pure routes).

| Field | EVM lowering | Lean |
|-------|--------------|------|
| `msg::sender` | `msg.sender` (`address`) | yes |
| `msg::value` | `msg.value` (`uint256`) | yes |
| `msg::timestamp` | `block.timestamp` | yes |
| `msg::createdAt` | `block.timestamp` (alias) | no (L16) |
| `msg::logicaltime` | `block.number` | no (L16) |
| `msg::int` / `msg::ext` | `false` / `true` (every EVM entry point is external) | no (L16) |

## System Context (`sys::`)

Available inside route bodies and member transforms (but NOT in pure functions or pure routes).

| Field | EVM lowering | Lean |
|-------|--------------|------|
| `sys::now` / `sys::timestamp` | `block.timestamp` | yes |
| `sys::block_number` (alias `sys::logicaltime`) | `block.number` | yes (`block_number`) |
| `sys::chainid` | `block.chainid` | yes |
| `sys::address` | `address(this)` | yes |
| `sys::balance` | `address(this).balance` | yes |
| `sys::prevrandao` (alias `sys::rnd_seed`) | `uint256(block.prevrandao)` | no (L16) |
| `sys::coinbase` | `block.coinbase` | no (L16) |
| `sys::basefee` | `block.basefee` | no (L16) |
| `sys::blobbasefee` | `block.blobbasefee` (EIP-4844) | no (L16) |
| `sys::gas_left` | `gasleft()` | no (L16) |
| `sys::origin` | `tx.origin` — same EOA-vs-contract caveats as in Solidity | no (L16) |
| `sys::gasprice` | `tx.gasprice` | no (L16) |

On Lean, `sys::balance`, `sys::block_number` and `sys::chainid` are read from
the `World`, so a route that uses them takes the world as an argument.

## Standard library (`std::`)

Cross-target pure helpers live under the `std::` namespace (always in scope).
**Bare** calls such as `min(a, b)` or `sha256(data)` are no longer valid — use
`std::math::min(a, b)` and `std::crypto::sha256(data)`.

Strict typing: string↔number conversion is **never** implicit; use
`std::str::parse_*` (returns `Option<T>`; unwrap with `match` or a `pure fn` bridge) and
`std::str::format(fmt, ...)`. Aliases: `parse_uint` / `parse_int`.

```cambrian
match std::str::parse_u64(s, 10) {
    some(n) => n,
    none => 0,
}
```

Authoritative tables, migration rules, and implementation status:
[STDLIB.md](STDLIB.md).

## Phased Routes

### Problem

In the standard (unphased) model, all member transforms are computed from the
pre-route state and applied atomically, and all effects execute afterward.
This works for most contracts, but it cannot express a route that asks another
contract for a value and then updates state from the answer, or one that must
update state between two external calls.

### Solution: Named Phases

A route body can be partitioned into **named phases**. Each phase defines a boundary
where member transforms are applied and effects execute. Phases are ordered by their
declaration order in the route body.

```cambrian
entity Token {
    routes {
        #[factory_only]
        constructor() => []
        view balanceOf(owner: address) -> U256 => [
            return(m_supply)
        ]
    }
    m_supply: U256 {
        in constructor() => 0
    }
}

entity Vault {
    routes {
        #[factory_only]
        constructor(token_addr: Address<Token>) => []

        withdraw(who: address) => [
            fetch: [
                var bal = balanceOf(who) ~> m_token;
            ]
            act where bal > 0 : throw 401: []
        ]
    }

    m_token: Address<Token> {
        in constructor(token_addr) => token_addr
    }

    m_last_seen: U256 {
        in constructor() => 0
        in withdraw(who) => act: bal
    }
}
```

`fetch` captures the answer of `Token.balanceOf`; `act` checks it and stores it.

### Execution Model

For a route with phases P₁, P₂, ..., Pₙ:

```
for each phase Pᵢ (in declaration order):
    1. Compute transforms: for each member m with a transform at Pᵢ,
       compute new value using state_{Pᵢ₋₁} (all members see pre-phase state)
    2. Apply transforms: update state atomically → state_Pᵢ
    3. Execute effects: run effects of Pᵢ (effects see post-transform state_Pᵢ)
```

Members without a transform at phase Pᵢ carry forward their value from Pᵢ₋₁.

### Temporal References in Phases

Inside a phase's transforms, `m_a` refers to the pre-phase value (state_{P-1})
and `^m_a` refers to the post-phase value (state_P). This is the same semantics
as unphased routes, but scoped to the current phase.

```cambrian
m_a: u64 { in foo(b) =>
    update:   b              // m_a = old_value, ^m_a = b
    finalize: m_a + 1        // m_a = b (from "update"), ^m_a = b + 1
}
```

### Multiple Members Across Phases

```cambrian
m_a: u64 { in foo(b) =>
    s1: b
    s2: m_a + m_c     // m_a = b (from s1), m_c = post-s1 value
}
m_c: u64 { in foo(b) =>
    s1: m_a + m_c     // m_a = initial, m_c = initial (pre-s1)
    // s2: not defined → m_c keeps its s1 value
}
```

### Phase Declaration Rules

1. **Route body is the source of truth** — phases and their order are defined
   exclusively in the route body using `tag: [actions]` syntax.
2. **Transforms reference phases** — member transforms use `tag: expr` to
   specify which phase they belong to.
3. **Explicit empty phases** — a phase with transforms but no effects must
   still appear in the route body as `tag: []`.
4. **Backward compatibility** — a route without phase tags uses the current
   single-phase semantics. No changes needed for existing code.

### Validation Errors

| Situation | Error |
|-----------|-------|
| Transform uses tag not in route body | `unknown phase 'X' in route 'foo', known phases: A, B, C` |
| Route has tags, transform has no tag | `transform for 'm_a' in phased route 'foo' must specify a phase` |
| Route has no tags, transform has tag | `phase tag 'X' in transform, but route 'foo' has no phases` |
| Untagged action in phased route body | `untagged action in phased route 'foo'` |
| Route-level `where` references a `var` | `V27` — `where clauses on E.r cannot reference var '<name>'` |
| Per-phase `where` references same- or later-phase `var` | `V28` — `var '<name>' is not available at this point` |

### Var Capture and Cross-Phase Conditions (EVM)

On EVM, a phase action may capture the synchronous return value of an
external call into a `var`:

```cambrian
fetch: [
    var bal = balanceOf(who) ~> m_token;
]
```

The `var` is in scope from the moment its `~>` runs and persists through
all later phases of the same route invocation. Two places can read it:

1. **Member transforms tagged at a later phase** — already supported.
2. **Per-phase `where` clauses on a later phase**, written as
   `phaseName where (cond) : throw N : [ ... ]`:

```cambrian
withdraw(who: address) => [
    fetch: [
        var bal = balanceOf(who) ~> m_token;
    ]
    act where bal > 0 : throw 401: [
        // ...transforms and effects...
    ]
]
```

The condition lowers to a `require(...)` placed at the **top of the
phase block**, after the previous phase has fully run (so any earlier
`var` is already bound) and before this phase's transforms or effects.

Scoping rules:

- A per-phase `where` on phase Pᵢ may reference `var`s defined in
  **strictly earlier** phases (P₁ ... Pᵢ₋₁), entity members, route
  parameters, and constants.
- A per-phase `where` may **not** reference a `var` defined in the same
  phase Pᵢ or in any later phase Pⱼ (j > i). The validator emits **V28**
  if you try.
- A **route-level** `where` clause runs before any phase, so it may
  **not** reference any `var`. The validator emits **V27** with a
  remediation hint to move the check into a per-phase `where` on a
  later phase. Use route-level `where`s only for conditions over route
  parameters and pre-existing state.

### Reentrancy and Phase Boundaries (EVM)

For an **unphased** route, the EVM backend guarantees that all
member-update SSTOREs are flushed **before** any external call (`~>`).
This preserves the checks-effects-interactions pattern and lets you
write idiomatic unphased Cambrian without losing reentrancy safety.

When you reach for phases on EVM, you typically do so for one of two
reasons:

- You need a synchronous return value (`var x = msg(args) ~> dest`)
  that subsequent transforms or wheres depend on.
- You need an interleaving of effects with state changes that the
  unphased model cannot express.

Otherwise, prefer the unphased form. The fewer phases a route uses, the
easier it is to read and audit.

### Expressiveness

Any imperative program with N statements can be encoded as a phased route
with at most N phases (one phase per statement). This means:

- The phased model is **as expressive as imperative programming** for the subset
  relevant to smart contracts (bounded, no unbounded loops).
- In practice, contracts need 1–3 phases. The single-phase model (current syntax)
  covers the vast majority of cases.
- The phased model is a **strict generalization**: unphased routes are phased routes
  with exactly one anonymous phase.

### Phased execution on EVM and Lean

Phased routes still lower on `--target evm` (Solidity phase blocks with
SSTORE / CALL ordering) and `--target lean` (world-threaded phase
assemblers).

### EVM-target intrinsics (`evm::`)

The `evm::` namespace exposes EVM-specific primitives. They lower to direct
Solidity built-ins or precompile calls, and split into a *pure* group
(deterministic, no state read or write — usable inside `pure fn`) and an
*impure* group (reads live state / block context). Every intrinsic except
`ecrecover` returns `U256`: hashes and block hashes are widened to `uint256`
so they compose with Cambrian arithmetic.

| Intrinsic | Pure? | Solidity lowering |
|-----------|:-----:|-------------------|
| `evm::ecrecover(hash, v, r, s) -> address` | yes | `ecrecover(bytes32(hash), uint8(v), bytes32(r), bytes32(s))` (returns `address(0)` on a bad signature) |
| `evm::keccak256(x) -> U256` | yes | `uint256(keccak256(abi.encode(x)))` (same as `hashOf(x)`) |
| `evm::keccak256Packed(a, b, ...) -> U256` | yes | `uint256(keccak256(abi.encodePacked(a, b, ...)))` |
| `evm::sha256(a, ...) -> U256` | yes | `uint256(sha256(abi.encodePacked(a, ...)))` (precompile 0x02) |
| `evm::ripemd160(a, ...) -> U256` | yes | `uint256(uint160(ripemd160(abi.encodePacked(a, ...))))` (precompile 0x03) |
| `evm::balance(addr) -> U256` | no | `addr.balance` |
| `evm::blockhash(n) -> U256` | no | `uint256(blockhash(n))` |

The canonical fixture is the EIP-2612 `permit(...)` route in
`examples/uniswap-v2/ERC20.cam`, which combines `evm::ecrecover` and
`evm::keccak256Packed` with an `m_nonces: HashMap<address, u64>`
member to recover the signer of an EIP-712 typed-data digest. The `evm::`
namespace has no Lean lowering (**L16**). Inside a `pure fn`, the impure
intrinsics (`evm::balance`, `evm::blockhash`) are rejected with **V4**; the
pure intrinsics in the table above are explicitly allowlisted.

> Prefer `std::crypto::sha256` for cross-target digests. On EVM-only call sites,
> `evm::sha256` remains available for explicit platform intent.

## Address Calculation

Entity addresses are computed using `Entity.address(args...)` dot-syntax:

```text
// As destination in send
Transfer(amount) ~> Account.address(m_email, m_user_id)

// In from clause (separate syntax, uses entity name directly)
from Account(m_email, m_user_id) : throw 403
```

`addressOf(Entity.state(args...))` is an alternative spelling that goes
through the entity's state form. The two forms are byte-equivalent on every
target — `Entity.address(args)` and `addressOf(Entity.state(args))` lower
to the same address expression — so use whichever reads better in context.

### EVM target — deterministic addresses

On the EVM target, the backend lowers `Entity.address(args)` and
`addressOf(Entity.state(args))` to a `CREATE2` address derived from:

- the contract's deploy bytecode (with constructor args = `identity` fields),
- a per-deployer factory salt of `0`,
- and a fixed `CambrianFactory` address that the project also generates.

Because the factory's own address is itself deterministic and known at
generation time, every contract in the project can predict every other
contract's address from its `identity` fields. Singletons (entities with no `identity`
members) take no arguments: `GlobalRegistry.address()` is the same call
on every node.

Deterministic mode also rewrites `from Entity(args)` checks to compare
`msg.sender` against the same `CREATE2` expression, so authentication
and addressing stay in sync.

## Deploy

`deploy Entity(args)` deploys a new instance of an entity from a route body.
The arguments are the target's `identity` members followed by the parameters
of its init route (**V32** checks the arity):

```cambrian
entity Child {
    identity m_id: u64

    routes {
        #[factory_only]
        constructor() => []
    }
}

entity Parent {
    routes {
        #[factory_only]
        constructor() => []

        spawn(id: u64) => [
            deploy Child(id)
        ]
    }
}
```

The new instance's address is `Child.address(id)`. Deploying the same
arguments twice in one route is rejected (**V45**).

### EVM target — deploy via the auto-generated factory

In deterministic mode the EVM backend auto-generates a `CambrianFactory`
contract per project. `deploy Entity(args)` lowers to a call into
that factory, which:

1. Performs a `CREATE2` deployment using the entity's `identity` arguments
   as constructor args (so the resulting address matches
   `Entity.address(args)`), forwarding any attached `value` to the new
   instance (`new Entity{salt: …, value: msg.value}(…)`), and
2. Calls a generated `initialize(non_identity_args...)` function on the
   freshly-deployed contract for any non-`identity` constructor
   parameters.

`deployX` is **not** permissionless: only the factory **owner** (the address
that constructed `CambrianFactory`, used for bootstrap / tests) or addresses
previously deployed by this factory (`isDeployed[msg.sender]`) may call it.
EOAs therefore cannot occupy identity-only CREATE2 slots; child deploys go
through entity routes that the factory already knows.

`initialize()` is guarded by `require(msg.sender == _factory, "only factory")`
followed by `require(!_initialized, "already initialized")`, so callers
cannot bypass the factory or re-initialize an existing contract. Authors
never write a hand-rolled factory.

Emitted `interface IEntity` marks a route `payable` when it is declared
`accept` **or** when its body / transforms read `msg::value`, matching the
implementation so typed sends with `{ value: … }` compile.

### Lean target — deploy, transfer, and fail surfaces

The Lean `World` mirrors EVM CREATE2 / balance semantics for the abstract model:

- **Occupancy.** Each entity slot has a `{entity}_deployed : Identity → Bool`
  flag. `deploy Entity(args)` checks occupancy and **throws** (ThrowCode `91`)
  on collision rather than overwriting — matching a second CREATE2 at the
  same salt.
- **Init params.** Non-identity constructor arguments are projected into the
  installed `State` via the target's init-route member transforms (e.g.
  `in constructor(initial) => initial` → `m_balance := initial`), matching
  EVM's factory `initialize(...)`.
- **Raw transfers.** `~> dest with { value: V }` always fail-propagates
  (underfunded transfer throws ThrowCode `90`); there is no silent
  `toOption.getD` recovery.
- **Dynamic dispatch.** `msg(args) ~> dest` where `dest` is `Address<E>` or
  a plain `address` lowers through `Cambrian.Generated.Dispatch.*` as
  `Except ThrowCode (World × T)` (void: `Except ThrowCode World`). The
  caller is a fail surface; value-carrying dispatch never uses `getD`.
- **L9 / L11.** Capturing a failing same-entity route without a fail surface,
  or fire-and-forget self-send to a failing route, is rejected by the
  validator and forced codegen emits a non-compiling `-- L9:` / `-- L11:`
  sentinel (not `Cambrian.exceptGetD`). Nested `call` wrappers auto-promote
  to fail-mode (they bind with `←`).
- **`inst.address`.** Lowers to `(<E>.address inst)` — the same CREATE2 helper
  as `Entity.address(args)` and `sys::address` (LEAN-H11).

### Lean target — `sorry` policy

Cambrian generates Lean **theorem statements** for `test` / `property` /
`invariant`; this repository does **not** prove those main goals.

| Placement | Policy |
|-----------|--------|
| Main theorems / lemmas in `*Spec.lean` | `sorry` in the proof body is **expected** (`:= by sorry` or a ladder ending in `\| sorry`) |
| `def`s, terms, other non-proof contexts | **No `sorry`** — fail closed instead of planting a hole in model code |
| Small helper lemmas (reflection, simp support) | May carry **real, simple, often auto-generated** proofs (P6) |

`lean: { proof_helpers: false }` forces statement-only `:= by sorry` on main
theorems. Numeric overflow modelling is selected with `lean.numerics`
(`overflow-wrap` default, `overflow-panic`, or `nat`) — see
[Lean numerics](#lean-numerics-leannumerics) under Expressions.

## Comments

```cambrian
// Line comment (only style supported)
```

## Keywords

```text
pure fn entity record enum const macro routes type library using
if else let in as match for init accept identity deploy private call use
where throw return with emit event error extern
view from some none array skip
test fuzz property invariant expect effects
true false
```

## Operator Precedence (lowest to highest)

| Level | Operators | Associativity |
|-------|-----------|---------------|
| 1 | `\|\|` | Left |
| 2 | `&&` | Left |
| 3 | `\|` (bitwise) | Left |
| 4 | `^` (bitwise xor) | Left |
| 5 | `&` (bitwise and) | Left |
| 6 | `==` `!=` | Left |
| 7 | `<` `<=` `>` `>=` | Left |
| 8 | `<<` `>>` | Left |
| 9 | `+` `-` | Left |
| 10 | `*` `/` `%` | Left |
| 11 | `!` `-` `*` (unary) | Prefix |
| 12 | `.field` `.method()` `[index]` `as Type` | Left (postfix) |

## Transpiler CLI

```
cambrian-transpiler <input.cam> [-o <output_dir>] [--dump-ast] [--source-map] [--target evm|lean]
```

| Flag | Description |
|------|-------------|
| `-o <dir>` | Output directory (default: `build/<entity_name>-entity/`) |
| `--dump-ast` | Print parsed AST in readable Cambrian format and exit |
| `--source-map` | Emit `.cam.map` JSON alongside generated code |
| `--target` | Compilation target: `evm` (default) or `lean` |
| `--check-lean` | After emitting a Lean project, run `lake build` |
| `--project <yaml>` | Multi-file project (`sources:`, `target:`, …) |

### Generated output

**EVM** (`--target evm`): a flat Solidity contract per entity (`^0.8.24`) plus
Foundry tests when the sources contain `test` / `fuzz` / `invariant`.
Deterministic mode emits `CambrianFactory` and splits constructors into a
deploy-time stub + factory-guarded `initialize()`.

**Lean** (`--target lean`): a Lake project — `Cambrian/Prelude.lean`,
`Cambrian/Generated/<E>.lean`, `<E>Routes.lean`, `World.lean`, `<E>Spec.lean`.

**Init semantics:**
- If an entity defines an `init` route, other routes require initialized state.
- If an entity has **no** `init` route, it is ready after deploy with defaults.

### Source maps

`--source-map` emits JSON mapping generated lines back to `.cam` lines.

---

## Codegen Mapping Summary

This snapshot emits **Solidity** (`--target evm`) and **Lean** (`--target lean`).
EVM-specific lowering is documented in the subsections above (tagged-union
enums, CREATE2, `payable` auto-detection). Lean layout is listed above under
Codegen targets; the `sorry` policy under [Lean target — `sorry`
policy](#lean-target--sorry-policy).

### Solidity-reserved name mangling

A Cambrian identifier that collides with a Solidity keyword or
built-in (`bytes`, `string`, `address`, `bool`, `uint`, `int`, `now`,
`type`, `function`, `receive`, `fallback`, `mapping`, `enum`, ...) is
auto-mangled with a single leading underscore at every declaration and
use site by `sol_sanitize_ident` (Phase EVM-15 Cluster C). The mangling
is symmetric: the entity author keeps writing the natural Cambrian
name (`receive`, `bytes`, ...) and the EVM backend rewrites both the
declaration and every reference to `_receive`, `_bytes`, ... so the
generated Solidity compiles without surprises. The mangling never
touches the AST or other backends — it lives entirely inside the EVM
codegen layer.

### `payable` auto-detection (EVM)

The EVM backend automatically tags a route as `payable` when any of
the following reads `msg::value`:

- the route body itself (any action, including `let` initialisers and
  the destination / args / options of a send),
- one of the route's `where` clauses,
- a member transform fired by the route (transitively — the analyser
  walks each `in <route>` body),
- a macro called from any of the above.

This is Phase EVM-15 H5. The user never writes `payable` explicitly;
the `accept` modifier is the cross-target opt-in for cases where the
route doesn't read `msg::value` but still needs to receive native
asset (e.g. a no-op `deposit()`).

---

## Testing

Cambrian has a built-in test language written alongside contract code in
`.cam` files. On `--target evm` it lowers to Foundry (`forge test`). On
`--target lean` it lowers to theorem statements in `<E>Spec.lean`.

### Syntax

```text
test "test name" for EntityName with { field: value, ... } {
    // optional context setup
    msg { sender: 0x000000000000000000000000000000000000a11c, value: 100 }
    sys { now: 1000 }

    // variable bindings
    let x = 42

    // call a route
    call routeName(arg1, arg2)

    // assertions (must follow a call)
    expect state { m_field: expected_value }
    expect throw 100
    expect return 42
    expect effects [~> 0x000000000000000000000000000000000000b0b0, ..]
    expect effects []                             // no effects asserted
}
```

### Structure

| Element | Description |
|---------|-------------|
| `test "name" for Entity` | Test declaration; `Entity` must be defined in scope |
| `skip from` | Optional modifier: skip all `from` clause sender verification in this test |
| `with { field: value }` | Optional initial state setup; fields must be valid members |
| `let name = expr` | Variable binding; available throughout the test |
| `msg { field: value }` | Set message context fields (sender, value, etc.) |
| `sys { field: value }` | Set system context fields (now, balance, etc.) |
| `call route(args)` | Invoke a route; route must exist with matching arity |
| `expect state { ... }` | Assert state fields after a call |
| `expect throw N` | Assert the call throws error code N |
| `expect return E` | Assert the call returns expression E |
| `expect return == E` | Same equality assertion as `expect return E` |
| `expect <bool>` | Assert an arbitrary boolean after the call (see [Properties](#properties-and-their-test--fuzz-instances)) |
| `expect effects [...]` | Assert effects produced by the call |

On the **Lean** target, every `expect` that follows a `call` is kept: each is
bound as a `Prop` while that call’s return/`w` is live, and the theorem goal is
their conjunction (`P₁ ∧ P₂ ∧ …`). The same applies to `property` bodies.
Invariants already conjoin every `check` the same way.

### Lenses (field path access)

State fields and return values support dot/index access for nested data:

```cambrian
// Record field inside HashMap
expect state { m_pools[0].reserve_a: 1500 }

// Nested HashMap
expect state { m_allowances[owner][spender]: 500 }

// Multiple fields in one expect
expect state {
    m_pools[0].reserve_a: 1000,
    m_pools[0].reserve_b: 500,
    m_count: 1
}

// Tuple index on state
expect state { m_pair.0: 10 }
```

### Return value assertions

```cambrian
// Scalar return
expect return 42

// Tuple return (full match)
expect return (0, 1000, 9999)

// Return lens — access individual element
expect return.0 == 0
expect return.field_name == 42

// Vec return — length and index
expect return.len == 3
expect return[0] == 42
expect return[0].name == 42

// Relational return checks are predicates, not the equality form above
expect return != 0
expect return > amount
expect return.field > 0
```

### Enum and Option assertions

```text
// Enum variant in state
expect state { m_state: State::Funded }

// Option values
with { m_resolution: none }
expect state { m_resolution: some("resolved") }
```

Known limitations of the current test lowering: on EVM, `expect state`
with a field path, a map key, an enum / `Option` value or a whole-map literal,
and `expect return.0`, do not yet compile; on Lean, `expect return.len` and
`expect return[0].x` do not yet build. A `with { ... }` block takes plain
values; record literals are not accepted there.

### Effect assertions

`expect effects [...]` lists effects of the preceding call; `..` at the end
of the list means "and possibly more". A `~> dest` element is a value
transfer: on EVM the harness checks that `dest`'s balance did not decrease
across the call, on Lean it becomes a modelled-transfer proposition.
`expect effects []` asserts nothing.

```cambrian
expect effects [~> seller, ..]
```

### Testing routes with `from` clauses

Routes with `from` clauses verify the sender address. To test only the
business logic, add `skip from`:

> **Modifier order:** `test "..." for <Entity> skip from with { ... }` —
> the `skip from` modifier sits between the entity name and the
> optional `with { ... }` initial-state block. `skip from <Entity>` is
> not a valid form.

```cambrian
test "ping increments counter" for Vault skip from with { m_id: 42 } {
    call constructor()
    call acceptPing(0x000000000000000000000000000000000000aabb)
    expect state { m_ping_count: 1 }
}
```

The Lean target does not honour `skip from` yet (**L7** warning): the `from`
checks stay in the generated theorems.

### Multi-step tests

Tests can contain multiple `call` / `expect` sequences. State is threaded automatically — the output state of one call becomes the input for the next:

```cambrian
test "multi-step" for Counter with { m_count: 0 } {
    call increment()
    expect state { m_count: 1 }

    call increment()
    expect state { m_count: 2 }
}
```

State is also propagated after calls without explicit `expect` blocks.

### HashMap assertions

HashMap fields can be asserted with literal syntax:

```cambrian
test "balances" for Token with { m_balances: {} } {
    call transfer(alice, bob, 100)
    expect state { m_balances: { alice => 900, bob => 100 } }
}
```

Individual HashMap keys can be checked via lenses:

```cambrian
expect state { m_balances[alice]: 900 }
```

### Running tests

```bash
# EVM: transpile, then Foundry
cambrian-transpiler --project project.yaml
cd build && forge test

# Lean: Lake project (theorems; proofs are sorry by policy)
cambrian-transpiler --project project.lean.yaml --check-lean
```

## Properties (and their `test` / `fuzz` instances)

A **`property`** is Cambrian's abstract, parameterised statement about an
entity. It is the canonical construct for property-based testing and for the
Lean specification target. A property declares:

1. a **typed parameter list**, attached to the property *name* (before
   `for`), and
2. a **logical body** — context (`msg` / `sys`), `let` bindings, `assume`
   preconditions, `call`s, and `expect*` assertions.

Concrete *examples* (`test`) and randomised *sampling runs* (`fuzz`) are then
instantiated as **nested blocks** inside the property:

```cambrian
property "increment is monotonic" (amount: u64) for Counter with { m_count: 0 } {
    assume amount % 2 == 0          // logical precondition (flows to Lean)
    call increment(amount)
    expect state { m_count: amount }

    test "even-4" { amount: 4 }                  // concrete example
    fuzz "small"  { amount in 0..1000 }          // sampling instance
    #[runs(2000)] fuzz "wide" { amount in 0..=1000 } with { m_count: 5 }
}
```

A postcondition that is not equality is a boolean `expect`. The expression is
the ordinary Cambrian expression language (comparisons, `&&` / `||` / `!`,
arithmetic, field and index access, calls). It is legal in `property`, `test`,
and `fuzz` bodies.

```cambrian
property "transfer pays" (amount: U256, start: U256) for Token with { m_balance: start } {
    assume amount > 0
    call transfer(to, amount)
    expect return != 0
    expect return > amount
    expect m_balance >= start
    expect m_spent + m_balance == start
    expect return > 0 && m_balance >= amount
}
```

- `expect return E` and `expect return == E` stay equality assertions.
- `expect return != / < / <= / > / >=` and `expect return.field <op> …` are
  predicates. `expect return.field == …` stays an equality lens.
- A bare member name is the **post-state** field after that call. Bind the
  starting value (`with { m_balance: start }`, or a property parameter) to
  mention it. `with { m_balance: * }` still quantifies the initial value, but
  the name `m_balance` inside the predicate is the post-state field.
- When the assertion does not start with the `return` keyword, the preceding
  call's return value is the name `result`. That name is reserved in this
  `expect`: if a parameter, `let`, or member in scope is also called `result`
  and the predicate mentions it, validation reports **T38**.
- `expect state { … }` stays a list of equalities. Invariant action bodies stay
  `bound` / `assume` / `skip if` / `advanceTime` only.

Notes:

- **Params attach to the property name, before `for`.** The entity never
  takes parentheses — `for Counter`, not `for Counter(amount: u64)`.
- The **body holds only logical steps** (`msg`/`sys`, `let`, `assume`,
  `call`, `expect*`). Sampling `bound`s are **not** allowed here — they live
  in the `fuzz` instances. This separation is what keeps the Lean theorem
  free of fuzzing artefacts (see below).
- Each **instance** binds the property's parameters:
  - `test [name] { p: value, ... } [with { init }]` — every parameter must be
    fixed to a concrete value.
  - `[#[runs(N)]] fuzz [name] { p in lo..hi, ... } [with { init }]` — each
    parameter is constrained to a sampling range; omitted parameters default
    to the full type range.
- The property-level `with { ... }` is the default initial state; each
  instance may override or extend it with its own `with { ... }`.
- Instance attributes: `#[runs(N)]` (fuzz only), `#[tag("INV-…")]`, and
  `#[skip_from]` (disable sender verification for that instance).
- A property with **no instances** still produces output: a single
  full-range `fuzz` harness when it has parameters, or a deterministic `test`
  when it has none. The Lean target always emits one theorem per property.

### Logical vs. technical: `assume` vs. `bound`

`assume` and `bound` look similar but play different roles:

| Construct | Role | Where it lives | Reaches Lean? |
|-----------|------|----------------|:-------------:|
| `assume <bool expr>` | **Logical** precondition | property body | **yes** (as a `→` hypothesis) |
| `bound x in lo..hi`  | **Technical** sampling range | `fuzz` instance | **no** |
| `bound x in lo..=hi` | inclusive sampling range | `fuzz` instance | **no** |

The Lean backend reads the `property` directly and emits

```text
theorem increment_is_monotonic :
  ∀ (amount : BitVec 64),
  … assume hypotheses … →
  … goal …
```

quantifying over the *full* parameter range with only the genuine `assume`
preconditions attached. Sampling `bound`s never appear, so a property's Lean
theorem is exactly as strong as its logic — no weaker because of a fuzzing
range you picked for speed.

### Forall-ized starting state (the `*` marker)

Pinning every "uninteresting" field to a default in `with { ... }` is
boilerplate-heavy, and for the Lean target you usually want the *opposite* of
a fixed start: a statement that holds for **any** initial value. The `*`
marker inside `with { ... }` opts a starting-state target into universal
quantification:

```cambrian
// "reset zeroes the counter from ANY starting value"
property "reset zeroes from any start" for Counter with { m_count: * } {
    call reset()
    expect state { m_count: 0 }
}

// `with { * }` forall-izes every state member at once
property "holds for any state" for Counter with { * } {
    call reset()
    expect state { m_count: 0 }
}
```

#### Entity state vs. blockchain context: `with { ... }` and `ctx { ... }`

Entity state and the blockchain/message context are **separate records**,
mirroring the runtime split (`State` vs. `MsgCtx`/`SysCtx` on Lean). Entity-state fields live in `with { ... }`; blockchain/context
parameters (`msg::…`, `sys::…`) live in a dedicated `ctx { ... }` block. A
context marker inside `with { ... }` is rejected (`T22`).

```cambrian
// state pin in `with`, context forall in `ctx`
property "incr from any sender" (amount: u64) for Counter
    with { m_count: 0 }
    ctx { msg::sender: * } {
    call increment(amount)
    expect state { m_count: amount }

    fuzz { amount in 0..10 }
}

// context entries may also be pinned to a concrete value
property "incr from a fixed sender" (amount: u64) for Counter
    with { m_count: 0 }
    ctx { msg::sender: 0x1234, sys::now: 1000 } {
    call increment(amount)
    expect state { m_count: amount }

    fuzz { amount in 0..10 }
}
```

Forms accepted in `with { ... }` (entity state):

| Form | Meaning |
|------|---------|
| `field: v` | pin a state member to a concrete value |
| `field: *` | forall over a state member |
| `*` (bare) | forall over **all** state members (pins still win for listed fields) |

Forms accepted in `ctx { ... }` (blockchain/context):

| Form | Meaning |
|------|---------|
| `msg::x: v` / `sys::x: v` | pin a context parameter to a concrete value |
| `msg::x: *` / `sys::x: *` | forall over a context parameter |

Forall-izable context parameters: `msg::sender`, `msg::value`, `sys::now` /
`sys::timestamp`, `sys::chainid`, `sys::block_number`, `sys::balance`. On the
Lean target `msg::` fields seed the threaded `MsgCtx`, while `sys::` fields seed
the initial world (`sys::*` is read off the World in World mode, not a separate
`SysCtx`): `sys::now` / `sys::timestamp` → `block.timestamp`, `sys::block_number`
→ `block.number`, `sys::chainid` → `block.chainId`, and `sys::balance` →
`w.balances` at the entity's own address (`E.address inst`). For invariants,
`sys::balance` seeding applies to **single-entity** invariants only —
multi-entity invariants have no single "self", so `sys::balance` is left
unseeded there.

Per-target semantics:

| Target | A forall-ized target becomes… |
|--------|-------------------------------|
| **Lean** | a `∀`-bound variable seeding the initial world / `ctx` |
| **fuzz** (Foundry) | a randomly **sampled** input |
| **test** (concrete) | a forall **state** field is dropped (keeps its default unless the instance pins it; warns `W8`); a forall **context** param is dropped silently; a concrete `ctx { ... }` pin still applies |

For example, `with { m_count: * }` emits the Lean theorem

```text
theorem reset_zeroes_from_any_start :
  ∀ (m_count : BitVec 64),
  let w := … { Counter.State.default with m_count := m_count } …
  … = 0
```

while the fuzz harness samples `m_count` as an input and seeds the initial
state with it. A concrete `test` instance cannot quantify, so a forall field
left unpinned there falls back to its default (and the validator emits `W8`).

### Desugaring for the EVM backend

For every non-Lean target the transpiler **desugars** each property into the
existing `test` / `fuzz` shapes the Foundry backend already understands:

- a `fuzz` instance becomes a fuzz harness whose body prepends a
  `bound p in lo..hi` step for each ranged binding, then the property's
  logical body (so `assume` / `call` / `expect*` carry over);
- a `test` instance becomes a concrete test whose body prepends a
  `let p = value` for each binding, with `assume` preconditions dropped (the
  chosen values are presumed to satisfy them).

`bound` and `assume` therefore still lower to Foundry preconditions:

- **Foundry**: `assume` → `vm.assume(...)`, `bound` → `StdUtils.bound(...)`.

### Cross-target support

Fuzz tests are emitted on a best-effort basis to every backend the project
has enabled. The validator emits `W7` warnings for parameter types that a
particular backend cannot fuzz natively (rather than failing the build).

Currently supported parameter types on Foundry:

| Type           | Foundry |
|----------------|:-------:|
| `u8…u128`      | yes     |
| `i8…i128`      | yes     |
| `U256`         | yes     |
| `bool`         | yes     |
| `address`      | yes     |
| `bytes`        | yes     |
| `String`, collections, records | fallback (`W7`) |

### Project-wide configuration

Global fuzz parameters live under a top-level `fuzz:` block in
`project.yaml`:

```yaml
fuzz:
  runs: 256                # iterations per fuzz test (default 256)
  seed: 0                  # 0 = nondeterministic; non-zero pins the RNG
  shrink: true             # enable shrinking on failure (default true)
  max_local_rejects: 1024  # how many `assume` rejections per case
```

The values are written into Foundry's `[profile.default.fuzz]` (`runs`,
`seed`, `max_test_rejects`) inside the generated `foundry.toml`.

### Validation rules specific to properties

| Code | Binding | Meaning |
|------|---------|---------|
| `T10` | Universal | Property parameter names must not collide with entity members; a `let` must not shadow a parameter. |
| `T11` | Universal | `bound` is not allowed in a property body (move it into a `fuzz` instance); an instance binding must reference a declared property parameter (and only once). |
| `T12` | Universal | `assume` body must be a boolean expression. |
| `T13` | — (folded into `W7`) | *Not emitted as a separate code* — unsupported parameter types produce a `W7` fallback-strategy warning. |
| `T17` | Universal | An instance binding uses the wrong form for its kind — `test` requires `p: value`, `fuzz` requires `p in lo..hi`. |
| `T18` | Universal | A `test` instance must bind every property parameter to a concrete value. |
| `T19` | Universal | A forall marker `field: *` references a field that is not an entity member (applies to properties and invariants). |
| `T20` | Universal | A context marker `msg::x` / `sys::x` in a `ctx { ... }` block uses an unknown parameter (allowed: `msg::{sender,value}`, `sys::{now,timestamp,chainid,block_number,balance}`). |
| `T21` | Universal | A field is both pinned (`field: v`) and forall-ized (`field: *`) in the same `with { ... }`. |
| `T22` | Universal | A context parameter (`msg::x` / `sys::x`) was declared in `with { ... }` / `init { ... }`; context belongs in a dedicated `ctx { ... }` block (entity state and context are separate records). |
| `T23` | Universal | A context parameter is declared more than once in the same `ctx { ... }` block. |
| `T38` | Universal | Boolean `expect <expr>` is not boolean-shaped, mentions `result` on a route with no return type, or mentions `result` when that name is already a parameter, `let`, or member. |
| `T39` | Language Solidity | `expect state` reads a member with no public Solidity getter (`Vec` members, records holding a `Vec` / `HashMap`, or a map whose value is a `Vec`; `String` / `bytes` members are public and readable). Assert through a `view` route and `expect return` instead. |
| `W7`  | Targeted | Parameter type degrades to a default strategy on a particular backend. |
| `W8`  | Universal | A `test` instance leaves a property-level forall **state** field unpinned, so it falls back to the field's default (the concrete test cannot quantify). |

## Stateful invariant tests

Stateful invariants extend fuzz testing to **random call sequences**: instead
of one parameterised step the harness picks an action from a user-defined
pool, applies it to the entity, and re-evaluates a set of boolean predicates
after every successful call. They are intended to catch state-machine bugs
that single-call fuzz tests cannot reach.

```cambrian
invariant "count never exceeds bound" for Counter {
    init { m_count: 0 }

    // optional: rotated through `_ctx.sender`
    senders { 0x00000000000000000000000000000000000000aa, 0x00000000000000000000000000000000000000bb }

    action increment(amount: u64) {
        bound amount in 0..1000    // per-action precondition
    }
    action reset() { }

    check m_count <= 1_000_000_000
}

invariant "reset must succeed" for Counter
    #[fail_on_revert]              // any revert fails the trace
{
    action increment(amount: u64) { bound amount in 0..100 }
    action reset() { }

    check m_count >= 0
}
```

| Construct | Meaning |
|-----------|---------|
| `invariant "name" for Entity { ... }` | Top-level declaration, single-entity. |
| `init { m_field: ... }` | Optional initial **state** (same shape as `with { ... }` on tests). Supports the `*` forall marker (`m_field: *` / bare `*`) — `∀`-quantified on the Lean target; on the dynamic (Foundry) targets each forall **state** field is seeded with a fuzzer-random value (Foundry: `vm.random*` cheatcode). The sample is shaped to the member's type so the seeded storage slot is canonical — `address` masks to 160 bits and `bool` collapses to `0/1`, while wider/integer types take a full 32-byte word (masked to the declared width on read). Context params (`msg::*`/`sys::*`) are **not** allowed here — use `ctx { ... }` (`T22`). |
| `with { m_field: ... }` | Alias for `init { ... }`. **Identity members may also be assigned here** — the harness threads them straight into the entity's constructor before the trace starts, so `Entity.address(...)` calls in `check` predicates resolve to the deployed instance. Constants / pure expressions only. |
| `ctx { msg::x: …, sys::y: … }` | Optional blockchain/context record for the trace (separate from entity state). Concrete pins set the initial sender / timestamp / value (Foundry: `targetSender` / `vm.warp`). The `*` forall marker randomizes: `sys::now: *` seeds a random initial timestamp; `msg::sender: *` leaves Foundry's sender pool unrestricted (per-call fuzzing). On the Lean target both namespaces are modelled: `msg::` fields seed the threaded `MsgCtx`, `sys::{now,timestamp,block_number,chainid}` seed the initial world's `BlockEnv` (forall `sys::*` becomes a `∀`-bound `BitVec 64`), and `sys::balance` seeds `w.balances` at the entity's own address (single-entity invariants only — multi-entity has no single "self"). |
| `senders { addr1, addr2 }` | Optional pool of senders, rotated round-robin. Defaults to a single zero sender. |
| `action route(<params>) { bound|assume }` | Routes the harness may pick. Per-action body may only contain `bound`/`assume` preconditions. |
| `check <bool expr>` | One or more invariants, all conjoined and re-checked after every successful call. The expression uses the same **ComputeExpr** surface as route bodies, `pure fn`, and Lean spec checks (arithmetic, comparisons, `HashMap` iteration, library calls). On EVM, heavy forms (e.g. `m.keys().fold(...)`) lower to entity-side `_cam_inv_*()` `view` helpers; unsupported forms fail as **I17** *(Language Solidity)* rather than silently passing. Read-only entity routes called from `check` (e.g. `totalSupply()`) get an inferred Solidity `view` modifier when the route has no member transforms and no mutating actions (SD-01a), even if the source omits the `view` keyword. |
| `#[fail_on_revert]` | Optional attribute: when present, any revert during the trace fails the test (default: skip reverting steps and continue). On Lean, `runTrace` matches this schedule: without the attribute (or with `fail_on_revert: false` in project config), an `.error` step does **not** abort the rest of the trace. |

### Predicate lowering (EVM)

Invariant `check` clauses are **predicates**: the boolean expression is the
same **ComputeExpr** grammar as route bodies and `pure fn` (arithmetic,
comparisons, `HashMap` folds, `library` calls like
`TokenSpec::sum_balances(m)`). On EVM:

1. Heavy forms (e.g. `m.keys().fold(...)`) lower to entity-side
   `_cam_inv_<slug>_<n>() public view` helpers (access to `m_*_keys`); the
   Foundry invariant calls `_sut._cam_inv_…()`.
2. Read-only routes referenced from `check` (e.g. `totalSupply()`) get an
   inferred Solidity `view` modifier when the route has no member transforms
   and no mutating actions, even without the `view` keyword in source.
3. Unsupported predicate shapes fail validation as **I17** *(Language
   Solidity)* — Foundry, not Lean — instead of a
   silent `require(true)`. Multi-entity helpers are owned by a single
   instance; a hoisted check that spans two instances is I17.
   `trace::length` / `trace::count` / `trace::lastWas` stay **Inline**
   (handler counters); they are never copied into an entity `_cam_inv_*`
   helper.

Lean lowers the same ComputeExpr via `gen_expr` in `<E>Spec.lean`.

### Action preconditions: `assume` vs. `skip if`

A per-action body may gate the action on the current entity state, the
current world/context, **and** the trace so far. Two keywords with
distinct semantics are available:

| Keyword | Meaning | Effect when the condition is false |
|---------|---------|-----------------------------------|
| `assume <bool expr>` | **Exclude** the action from the trace space. The action only runs when `<expr>` holds. | The step is rejected: dynamic backends discard the case (`vm.assume` / `prop_assume!`); the Lean theorem only quantifies over *well-formed* traces (a `traceValid … = true →` hypothesis). |
| `skip if <bool expr>` | **No-op** the action. The action is still "taken", but does nothing when `<expr>` holds. | The harness advances to the next step without calling the route (the step counts toward trace length but leaves state unchanged). |
| `bound v in lo..hi` | Constrain a fuzz **parameter** to a numeric range. | The case is rejected (same as `assume`), keeping the parameter inside `[lo, hi)` (or `[lo, hi]` with `..=`). |

On the Lean target, `assume` is now lowered with full **exclude**
semantics (previously it was best-effort). The generated spec emits a
`stepValid` predicate (one arm per action, conjoining that action's
`assume` conditions) and a `traceValid` fold; the theorem gains a
`traceValid w inst ctx accInit trace = true →` hypothesis. When an
invariant has **no** `assume`, the output is byte-identical to before —
no `stepValid`/`traceValid` machinery is emitted.

### Trace-aware accessors (`trace::*`)

Inside an invariant's `assume` and `check` expressions you may reference
the trace executed so far via three built-in accessors:

| Accessor | Type | Meaning |
|----------|------|---------|
| `trace::length` | `Nat` / `uint256` | Number of actions executed strictly before the current evaluation point. In an `assume` this is the index of the action about to run; in a `check` it is the full trace length. |
| `trace::count(route)` | `Nat` / `uint256` | How many times `route` has run so far. `route` must be a bare name of a declared `action` in this invariant. |
| `trace::lastWas(route)` | `Bool` | `true` iff the immediately preceding action was `route` (`false` at the start of a trace). |

```cambrian
invariant "deposits lead withdrawals" for Vault {
    init { m_balance: 0 }

    action deposit(amount: u64) {
        bound amount in 1..100
        assume trace::length < 10                       // cap the trace
    }
    action withdraw(amount: u64) {
        bound amount in 1..100
        assume m_balance >= amount                      // state condition
        assume !trace::lastWas(withdraw)                // no two in a row
        assume trace::count(withdraw) <= trace::count(deposit)
    }

    check m_balance >= 0
    check trace::count(deposit) >= trace::count(withdraw)
}
```

The accessors are sugar over a per-trace accumulator that is **only
emitted when actually referenced**:

- **Lean**: a minimal `TraceAcc` structure (only the referenced
  `len` / `count_<route>` / `last` fields), `accInit` / `accUpdate`, and
  (when a `check` reads `trace::*`) a `finalAcc` fold. The route
  transition (`step` / `runTrace`) is never touched.
- **Foundry**: handler storage counters (`_traceLen`,
  `_traceCount_<route>`, `_traceLast_<route>`) bumped after each
  successful action call.

`trace::*` accessors are restricted to `assume` and `check` (see `I15` /
`I16`); they are rejected in `bound` endpoints, `skip if`, `derived`
bodies, `track` bindings, and outside invariants, so the route
transition stays pristine.

### Cross-target support

Invariants are emitted to Foundry on `--target evm` and to Lean theorems
on `--target lean`:

- **Foundry**: a per-invariant `Handler` contract wraps each action and
  applies `bound`/`assume`; an `Invariant_<name>Test` contract registers it
  via `targetContract` / `targetSelector` / `targetSender`. Each `check`
  becomes a `function invariant_<name>()` body using `require`.
- **Lean**: `Action` + `step` + `runTrace` in `<E>Spec.lean`.

### Project-wide configuration

```yaml
invariant:
  runs: 256                 # number of randomised traces (default 256)
  depth: 50                 # maximum sequence length per trace (default 50)
  fail_on_revert: false     # global default; per-invariant attribute wins
  seed: 0                   # 0 = nondeterministic; non-zero pins the RNG
  max_local_rejects: 1024   # max `assume` rejections per trace
```

These values are wired into Foundry's `[profile.default.invariant]`
section in the generated `foundry.toml`. Lean reads the same `invariant`
declaration for theorem statements (runs/depth do not apply).

### Multi-entity (system) invariants

A single invariant can drive a *system* of named instances spanning multiple
contracts, by listing the instance map in the header:

```cambrian
invariant "vault sum equals treasury total" for { v: Vault, a: Vault, t: Treasury }
    #[fail_on_revert]              // optional
{
    init v { m_balance: 0 }
    init a { m_balance: 0 }
    init t { m_total: 0 }

    // optional, global, rotated round-robin
    senders { 0x00000000000000000000000000000000000000aa, 0x00000000000000000000000000000000000000bb }

    action v.deposit(amount: u64) { bound amount in 0..1000 }
    action a.deposit(amount: u64) { bound amount in 0..1000 }
    action t.credit(amount: u64) { bound amount in 0..1000 }

    check v.m_balance + a.m_balance >= t.m_total
}
```

| Construct | Meaning |
|-----------|---------|
| `for { name: Entity, ... }` | Multi-entity header. Two or more instances of the same entity are allowed (e.g. `v: Vault, a: Vault`). |
| `init <inst> { field: value, ... }` | Per-instance initial state. Replaces the flat `init { ... }` block of the single-entity form. |
| `action <inst>.<route>(<params>) { ... }` | Action targeted at a specific instance. The `<inst>.` qualifier is required in the system form. |
| `check <inst>.<member> ...` | Member references in the predicate must be qualified with `<inst>.` to disambiguate between instances. |

The single-entity form (`for <Entity>`) keeps working unchanged; internally it
desugars to a one-instance system with `name = "_self"` so that the rest of the
codegen pipeline is uniform.

### Cross-target support for multi-entity invariants

| Backend       | Supported | Notes |
|---------------|-----------|-------|
| Foundry       | Yes       | Single `Handler_<name>` contract holds one storage slot per instance, deploys each one in `setUp()`, and exposes wrappers named `<inst>_<route>`. Checks lower `<inst>.<member>` to `_<inst>.<member>()`. |
| Lean          | Yes       | `Action` + `step` / `runTrace` in the owner entity's `<E>Spec.lean`. |

### Foundry handler additions

The Foundry backend supports a richer handler vocabulary. Extra constructs
that Foundry does not lower are ignored at codegen time (validator still
parses them as `TestStep`s).

```cambrian
invariant "borrowed never exceeds total assets" for LendingPair
    #[tag("INV-LEND-001")]    // audit-trail marker; surfaces in fn name
    #[runs(2000)]             // per-decl forge-config invariant.runs
    #[depth(80)]              // per-decl forge-config invariant.depth
    #[with_time]              // expose synthetic advanceTime(secs)
{
    init { m_total_assets: 1000, m_borrowed: 0 }

    track {
        let initial_total = m_total_assets    // captured once in handler
        let initial_borrowed = m_borrowed      // ctor; visible by name
    }                                          // in check / action / derived

    derived utilization() -> u128 {
        return (m_borrowed * 100) / m_total_assets
    }

    exclude senders { 0x0000000000000000000000000000000000000001 }
    exclude selectors { accrueInterest }       // excludeSelector(...)

    action deposit(amount: u128) {
        bound amount in 1..m_total_assets      // computed bound (T11 relaxed)
    }

    action withdraw(amount: u128) {
        skip if m_total_assets == 0            // if (cond) return; -- no
                                                // rejection counter bump
        bound amount in 1..m_total_assets
    }

    action accrueInterest() {
        advanceTime(3600)                      // vm.warp + vm.roll
    }

    check m_borrowed <= m_total_assets
    check utilization() <= 100                 // calls handler-side derived
    check m_total_assets >= initial_total      // refers to track binding
}
```

| Construct | Lowering on Foundry |
|-----------|----------------------|
| `#[tag("INV-XXX")]` | NatSpec `/// @dev tag: INV-XXX`, function-name suffix `_INV_XXX`, revert-message prefix `[INV-XXX] ...`. |
| `#[runs(N)]` / `#[depth(N)]` | `/// forge-config: default.invariant.runs/depth = N` annotation directly above the generated `invariant_*` function. |
| `#[with_time]` | Synthetic `advanceTime(uint256 secs)` action exposed on the handler + added to `targetSelector`. |
| `track { let name = expr; ... }` | One `<T> public name;` field per binding on the handler; constructor captures the value once. Bare `name` references lower to `_handler.name()` in checks. |
| `derived name(p: T) -> T { ... return e; }` | `function name(...) public view returns (T) { ... return e; }` on the handler; bare `name(args)` lowers to `_handler.name(args)`. Validator I12: body may only contain `let` bindings. |
| `skip if cond` | `if (cond) return;` inside the action wrapper — early-exit without bumping the rejection counter, unlike `assume`. |
| `advanceTime(secs)` | `vm.warp(block.timestamp + secs); vm.roll(block.number + 1);` Lives inside an action body. |
| `exclude senders { addr1, addr2 }` | One `excludeSender(...)` per address in `setUp`. |
| `exclude selectors { route1, route2 }` | A FuzzSelector entry passed to `excludeSelector(...)` containing the named action wrappers. Validator I14: every name must match a declared action (or `advanceTime` when `#[with_time]` is on). |
| `bound x in lo..hi` (computed) | `lo` / `hi` may now reference entity members and snapshot bindings; threaded through `substitute_member_accessors`. T11 still requires `x` to be an action parameter. |

### Tiered Foundry profiles

`generate_foundry_toml` always emits two extra profiles alongside
`[profile.default]`:

```toml
[profile.cambrian]                # cheap CI cycle
[profile.cambrian.fuzz]
runs = 100
[profile.cambrian.invariant]
runs = 50
depth = 50

[profile.cambrian_night]          # overnight stress
[profile.cambrian_night.fuzz]
runs = 10000
[profile.cambrian_night.invariant]
runs = 5000
depth = 250
```

Override per-profile knobs in `project.yaml`:

```yaml
foundry:
  profiles:
    cambrian:
      fuzz_runs: 200
      invariant_runs: 100
    cambrian_night:
      invariant_runs: 20000
      fail_on_revert: true
```

Run the suite via `FOUNDRY_PROFILE=cambrian forge test` (CI) or
`FOUNDRY_PROFILE=cambrian_night forge test` (nightly).

### Validation rules specific to invariants

| Code  | Binding | Meaning |
|-------|---------|---------|
| `I1`  | Universal | Each `action <name>` must reference an existing route on the entity. |
| `I2`  | Universal | Action parameter list must match the route signature in arity and types. |
| `I3`  | Universal | Each `check` expression must look like a boolean. |
| `I4`  | Universal | Action body may only contain `bound`/`assume`/`skip if`/`advanceTime(...)`. |
| `I5`  | Universal | Invariant must declare at least one `action` and at least one `check`. |
| `I6`  | Universal | `senders { ... }` entries must be address/pubkey literals or constant identifiers. |
| `I8`  | Universal | Instance names must be unique within an invariant. |
| `I9`  | Universal | Each instance's entity must exist; every `action <inst>.<route>` must reference a declared instance. |
| `I10` | Universal | Per-instance `init <inst> { field: ... }` keys must be members of the corresponding entity. |
| `I11` | Universal | In a multi-entity invariant, `check`/`bound`/`assume` member references must be qualified with `<inst>.<member>` (bare names are ambiguous). |
| `I12` | Universal | `derived <name>(...) -> T { ... }` body may only contain `let` bindings; queries are pure view helpers. |
| `I13` | Universal | Names declared inside `track { let name = expr; ... }` must be unique. |
| `I14` | Universal | Every name listed in `exclude selectors { ... }` must match a declared `action` (or `advanceTime` when `#[with_time]` is on). |
| `I15` | Universal | `trace::count(route)` / `trace::lastWas(route)` must name a declared `action` route in the invariant; the accessor name must be `length`, `count`, or `lastWas`. |
| `I16` | Universal | `trace::*` accessors are only allowed inside an invariant's `assume`/`check`; rejected in `bound`/`skip if`/`derived`/`track`, routes, properties, and tests. |
| `I17` | Language Solidity | Invariant `check` uses an EVM predicate form that cannot be lowered (e.g. unsupported library/`keys().fold` shape, or a hoisted check spanning multiple instances). `trace::*` accessors are not I17 — they inline against handler counters. Rewrite as a `view` route if needed. Lean does not fire this rule. |
| `I18` | Domain EVM | Multi-instance invariant: identity-less duplicate instances, a duplicate identity tuple, or a cyclic instance deploy-order graph. |
| `V58` | Universal | `deploy { ... }` must contain exactly one address. |
| `V59` | Universal | `deploy { ... }` is forbidden on multi-entity (`for { … }`) invariants. |
| `W13` | Universal | Constructor bootstrap without explicit `deploy { ... }` uses legacy harness semantics (informational). |
| `W14` | Universal | Deploy address is also listed under `senders` (informational). |
| `W15` | Universal | `check` pins a member / view to a constant with `==` while a non-excluded `action` can decrease it (vacuous under fuzz). |

### Coverage-guided fuzzing

This snapshot uses Foundry's `forge test` / `forge invariant` pipeline.
Coverage-guided `cargo-fuzz` sidecars are not included.

Foundry's random-fuzz `forge invariant` runner is enabled by default for
the EVM target.

#### Project-config validation rules

| Code  | Binding | Meaning |
|-------|---------|---------|
| `F1`  | Packaging | Coverage knobs require the matching harness to be enabled in `project.yaml`. |
| `F2`  | Packaging | `lean.numerics` must be `nat`, `overflow-wrap`, or `overflow-panic` when set. |
| `F3`  | Packaging | Coverage engine names, when present, must be recognised. |
| `F4`  | Packaging | Imported library file declares `entity` / `test` / `fuzz` / `invariant` (list the file under `sources:` instead). |
| `F5`  | Packaging | A yaml `imports:` path or a bare in-language `import` did not resolve against the first search directory or any `library_paths` root. |

---

## Validation Rule Reference

Consolidated reference for every diagnostic code emitted by
`cambrian-transpiler`. Errors short-circuit codegen; warnings do not.
Test-, invariant-, and project-config-specific codes are cross-linked
to their full sections above.

Every table carries a **Binding** column — where the rule runs in the
(domain × language) dispatch matrix:

- **Universal** — kernel semantics; runs for every target and in target-less
  `--validate` mode.
- **Targeted** — runs for every concrete target, but needs to know which one
  (skipped in target-less mode).
- **Domain EVM** — world-model rules of the EVM domain, shared
  by every core on that domain (e.g. Domain EVM = `--target evm` *and*
  `--target lean`).
- **TVM construct** / **non-EVM** — rejection of one domain's constructs on
  every domain that does not model them.
- **Solidity** / **Lean** — carrier-language expressivity of a single core.
- **Lean-EVM pair** — contract of one specific (domain, language) adapter.
- **Packaging** — `project.yaml` checks, run by the project loader outside
  the rule registry.

### V — entity / route / member rules

| Code  | Severity | Binding | Meaning |
|-------|----------|---------|---------|
| `V1`  | error    | Universal | Member transform `in <route>` references an unknown route. |
| `V2`  | warning  | Universal | Route has no member transforms and an empty body (likely a stale stub). |
| `V3`  | error    | Universal | Temporal `^name` reference is unknown, or a temporal cycle exists between members within a single route. |
| `V4`  | error    | Universal | `pure fn` references state, `msg::*`, `sys::*`, temporals, macros, or impure `evm::*` intrinsics. |
| `V7`  | error    | Universal | Constant value is not a literal expression. |
| `V8`  | error    | Universal | Undefined identifier / function / macro / temporal reference. |
| `V9`  | error    | Universal | `view` route modifies state (has member transforms). |
| `V10` | error    | Universal | `pure` route modifies state (has member transforms). |
| `V11` | error    | Universal | `pure` route uses an impure construct (`msg::*`, `sys::*`, members, sends, deploys, effects, var calls, `call` to private routes, ...). |
| `V12` | error    | Universal | Entity has more than one `init` route, or an `init` route declares a return type. |
| `V13` | error    | Universal | Duplicate phase name within a phased route. |
| `V14` | error    | Universal | Member has unphased transform on a phased route, or a phased transform on an unphased route. |
| `V15` | error    | Universal | Member transform references an unknown phase tag. |
| `V16` | error    | Universal | Identity member declares transforms (identity fields are static, set at deploy time). |
| `V17` | error    | Universal | Identity member declares a default value. |
| `V20` | error    | Universal | Empty-string literal compared with an `address` value (`addr == ""`); use `msg::int` or compare with a real address. |
| `V21` | error    | Universal | `use <ns>` import names an unknown SDK namespace. |
| `V22` | error    | Universal | `<ns>::name(...)` used without the corresponding `use <ns>` import. |
| `V23` | error    | Universal | Named send / var call to an untyped `address`, or to an unknown route on the resolved entity (real or `extern`). |
| `V24` | error    | Universal | `var x = <route>(...) ~> dest` targets a route with no return type. |
| `V25` | error    | Universal | `var x = ...` shadows a route parameter or state member, or duplicates a previously defined `var`. |
| `V26` | error    | Universal | `var` referenced from a member transform that runs in the same or earlier phase as the var definition. |
| `V27` | error    | Universal | Route-level `where` clause references a `var` defined inside the body (vars don't exist yet at route entry — move the check into a per-phase `where`). |
| `V28` | error    | Universal | Per-phase `where` on phase `Pᵢ` references a `var` defined in the same phase or a later phase (only earlier-phase vars are in scope). |
| `V29` | error    | Universal | `for` loop body contains a forbidden action: capturing var-call (`var x = msg(args) ~> dest`), or `return(...)`. |
| `V30` | error    | Universal | `extern entity Name { ... }` is duplicated within the program, or collides with a real `entity Name`. |
| `V31` | error    | Universal | Duplicate route name inside an `extern entity` block. |
| `V32` | error    | Universal | `deploy Entity(args)` arity does not match the target entity's constructor surface (`identity_member_count + init_route_param_count`). Skipped for `extern entity` and unknown targets. |
| `V33` | error    | Domain EVM | `from Entity(args)` arity must match the target entity's identity-member count. Skipped for `extern entity` targets. |
| `V42` | error    | Universal | Route body contains `return(<value>)` but declares no `-> T` return type. A bare `return` / `return()` (no value) is a valid early exit and is exempt. |
| `V43` | error    | Universal | Temporal `^member` appears outside a member transform body (`where` / `from` / route action / member default). `^member` is only valid inside transforms (see Temporal References). |
| `V44` | —        | — | *Reserved* for if-scoped `let`/`var` escape (LEAN-ST-H3). |
| `V45` | warning / error | Universal | Route contains more than one `deploy Entity(...)` for the same entity name: **warning** when constructor-argument trees differ (and do not const-fold equal); **error** when any two are identical or const-fold to the same integer arguments (CREATE2 / occupancy collision; e.g. `Vault(0)` vs `Vault(0+0)`). |
| `V46` | —        | — | *Retired.* Untyped `let` bindings of address-shaped literals are no longer linted; use `as address`, a typed `let`, or call-site parameter typing. |
| `V47` | warning  | Universal | Match arm is unreachable after a catch-all (`_` / bare identifier), duplicates a prior literal / `none` / nullary enum variant arm, or is a catch-all after arms that already cover every case (all enum variants, `some` + `none`, `true` + `false`). Lean drops such a dead catch-all, which it would otherwise reject as a redundant alternative. |
| `V48` | error    | Universal | Expression `if` without `else` in a value-required context (transform body, `return` payload, `let` RHS, call/send/deploy args, match-arm body, …). Statement `if cond => […]` is exempt. |
| `V49` | error    | Universal | Bare stdlib call (`min`, `pow`, `sha256`, …) without `std::math::` / `std::str::` / `std::crypto::` prefix. |
| `V50` | error    | Universal | Member transform assigns incompatible types. String↔numeric mismatches require an explicit `std::str::parse_*` / `std::str::format` bridge. `address` vs `Address<Entity>` (and `Address<A>` vs `Address<B>`) require a typed-address conversion — diagnostics preserve the `Address<Entity>` spelling. |
| `V51` | error    | Universal | Unused `let` binding in a route body or `pure fn` body — reference the bound name in a later action/expression in the same scope, or remove the `let`. |
| `V52` | error    | Universal | Record type participates in a cycle of strict field references (no `Option` / `Vec` / `HashMap` indirection), **or** a type alias unfolds forever (`type A = B; type B = A`, `type A = A`, `type A = Option<A>`). Alias cycles are not broken by `Option`/`Vec`/`HashMap`. Such types are uninhabitable for default construction (Lean/EVM backends would recurse forever). |
| `V53` | error    | Universal | Nested pattern under `let some(<pat>)` — the inner pattern must be a plain identifier (or `_`). Nested patterns are not supported and would otherwise be emitted as a binder name. |
| `V54` | error    | Universal | Route body mixes phase blocks and bare actions (use a fully phased or fully unphased body). |
| `V55` | error    | Universal | `using` method name collides with a built-in for the receiver type (`Vec` / `HashMap` / `String` / `Address`). |
| `V56` | error    | Universal | `using { fn } for T;` or `using Lib for T;` references an undeclared `pure fn` or `library`. |
| `V57` | error    | Universal | `using` referenced `pure fn`'s first parameter type does not match the `for` type. |
| `V58` | error    | Universal | `deploy { ... }` in an invariant must contain exactly one address. |
| `V59` | error    | Universal | `deploy { ... }` is forbidden on multi-entity (`for { … }`) invariants. |
| `V60` | error    | Universal | Multiple `from` clauses on one route must share the same `: throw` annotation (no mixed numeric / named throw codes). |
| `V62` | error    | Domain EVM | `msg::sender` appears in the init/constructor route. Under factory CREATE2 deploy, `msg::sender` is the factory, not the external deploy caller — take the address as an explicit constructor parameter. |
| `V63` | error    | Domain EVM | Init/constructor route lacks `#[factory_only]`. Single-file mode validates the same CREATE2 model as a project; there `V62` / `V63` are warnings. |
| `V64` | error    | Universal | `#[factory_only]` appears on a route other than the init/constructor route. |
| `V65` | error    | Universal | Arithmetic `+` / `-` / `*` / `/` / `%` (and wrapping variants) where either operand infers `Option<_>`. |
| `V66` | error    | Universal | Non-exhaustive `match` without a `_` / binding arm: an enum variant, `some` / `none`, or `true` / `false` is not covered, or integer-literal arms have no catch-all. |
| `V67` | error    | Universal | Route declares `-> T` but never returns a value (and never throws), or a returned value / `pure fn` result has a different category (numeric / `String` / `bool`) than the declared type. |
| `V68` | error    | Universal | Record literal `R { … }` sets a field twice, names a field `R` does not have, or omits a field (use `base { field: v }` to update an existing value). |
| `V69` | error    | Universal | Arithmetic or comparison operator mixes a signed and an unsigned integer operand; cast one side with `as`. |
| `V70` | error    | Universal | Member transform `in r(a, b)` binds a different number of parameters than route `r` declares. Bind all of them (`_` for unused ones) or none (`in r()`). |

### E — EVM-domain / Solidity-language compatibility rules

Dispatch is by rule binding.
Despite the shared `E` prefix, these rules split into three groups:

- **TVM construct** (`E01`–`E06`, `E10`, `E11`, `E13`–`E15`, `E26`): a
  construct of the TVM message model (`gosh::*`, `msg::pubkey`/`currencies`/`body`,
  `sys::pubkey`/`seqno`, test `registry`, platform effects, bounce handling)
  used on `--target evm` or `--target lean`. These constructs are outside
  this reference; the rows below exist so the diagnostics can be looked up.
- **Domain EVM** (`E09`, `E16`, `E22`; also `V33`, `V36`, `V40`, `V41`):
  genuine EVM world-model rules, shared by every EVM-domain core.
- **Solidity** (`E07`, `E08`, `E17`–`E21`, `E23`): SolidityCore expressivity —
  fire only for `--target evm`, not `--target lean`.

`E12` is the mirror of the TVM-construct group: `evm::*` is rejected on
every non-EVM domain. (Lean is on the EVM domain; there `evm::*` is **L16**.)
Most warnings mean codegen still produces something runnable with caveats;
`E03`/`E04`/`E12`/`E13`/`E14`/`E15`/`E16`/`E22`/`E26` are hard errors.
Event/error rules **V34**/**V35**/**V38**/**V39** are universal; **V36**
(indexed-param limit) is Domain EVM (see Events, Custom Errors, and
Receive/Fallback above).

| Code  | Severity | Binding | Meaning |
|-------|----------|---------|---------|
| `E01` | warning  | TVM construct | `use gosh`: `gosh::*` namespace has limited EVM support; most calls become no-ops. |
| `E02` | warning  | TVM construct | `gosh::<name>(...)` action has no EVM equivalent (lowers to a no-op comment). |
| `E03` | error    | TVM construct | `msg::pubkey` is TVM-specific (no EVM equivalent). |
| `E04` | error    | TVM construct | `msg::currencies` is TVM-specific (ECC-7 currency map). |
| `E05` | warning  | TVM construct | `accept` on a regular route has no EVM effect (payable detection is automatic); `receive` still requires `accept` (`V40`) and is exempt. |
| `E06` | warning  | TVM construct | `gosh::setcode` / `gosh::setCurrentCode`: on-chain code upgrades not supported on EVM. |
| `E07` | error / warning | Solidity | **Error** on bare closure-as-value or bare `Range` outside `for` (not lowerable; mirrors Lean L2). **Warning** on unsupported iterator-method chains inside `for` / `.fold`. Fused `.filter().map().fold()` / `m.iter().fold()` still lower (EVM-12). |
| `E08` | warning  | Solidity | Payload-bearing user enum variant lowers to a tagged-union struct (one slot per payload position; see §EVM lowering: tagged-union enums above). |
| `E09` | warning  | Domain EVM | `from`-clause sender verification maps to `require(msg.sender == ...)` on EVM. |
| `E10` | warning  | TVM construct | Test `registry { ... }` block is TVM-specific, skipped on EVM. |
| `E11` | warning  | TVM construct | `expect effects [...]` references a TVM-specific platform effect, skipped on EVM. |
| `E12` | error    | non-EVM | `evm::<name>(...)` cannot be used when the target is not EVM. |
| `E13` | error    | TVM construct | `msg::body` is TVM-specific (raw inbound cell). |
| `E14` | error    | TVM construct | `sys::pubkey` / `sys::seqno` are TVM-specific. |
| `E15` | error    | TVM construct | `gosh::<name>(...)` in *expression position* has no EVM equivalent (move to action position or guard behind a target check). |
| `E16` | error    | Domain EVM | `<ns>::name(...)` namespace has no EVM lowering, or `address_of <Entity>(...)` was used where CREATE2 addresses are not emitted, or `encode<...>(...)` was used in entity / route bodies (test-only). |
| `E17` | error    | Solidity | HashMap member-transform body shape is unrecognised (would emit a no-op). Use `m.insert(k, v)` / `m.update(k, v)` / `m.remove(k)` (optionally inside `if`, `block`, or `let`). |
| `E18` | error    | Solidity | Tuple type used outside a multi-return position (would silently erase to `bytes`). Define a `record` instead. |
| `E19` | error    | Solidity | Generic type other than `Vec`, `HashMap`, `Option` has no EVM lowering. |
| `E20` | error    | Solidity | Bare type identifier does not resolve to a primitive, record, enum, or alias visible from EVM (would erase to `uint256`). |
| `E21` | error    | Solidity | `as <Type>` cast target is not in the supported set (`u8`–`U256`, `i8`–`i128`, `bool`, `address`, `pubkey`). |
| `E22` | error    | Domain EVM | Typed send targets an entity name that's neither in the project nor declared via `extern entity`. |
| `E23` | error    | Solidity | Mixed-type tuple destructure `let (a, b, …) = expr` cannot infer per-slot Solidity types from `expr`. Bind via separate `var` calls or return a `record`. |
| `E25` | error    | Domain EVM | `std::*` call is outside the standard-library allowlist (on EVM it would otherwise lower to a silent `0`). |
| `E26` | error    | TVM construct | `rescue` / `recover` bounce handling has no EVM/Lean analogue and no try/catch lowering. |
| `E27` | error    | Solidity | Expression codegen had no lowering for a shape and substituted the literal `0`. Raised after the files are emitted, so the artifact can be inspected. Known gap: `fold` over a tuple accumulator. |
| `E28` | error    | Domain EVM | Method with no Solidity or Lean lowering: `Option` `.unwrap()` / `.unwrap_or()` / `.expect()` / `.is_some()` / `.is_none()`, collection `.get()` / `.set()`. The message names the supported spelling. |
| `E29` | error    | Solidity | `view` route body contains a named send, value transfer, `deploy`, `emit`, or platform effect; Solidity rejects these in a `view` function. (Lean threads the world through views and accepts them.) |

### L — Lean-language / Lean-EVM-pair rules

LeanCore expressivity (`Language(Lean)`) and Lean×EVM adapter contracts
(`Pair(Evm, Lean)`). See also the quick list in `AGENTS.md`.

| Code  | Severity | Binding | Meaning |
|-------|----------|---------|---------|
| `L1`  | error    | Lean | Unsupported generic beyond `Vec` / `HashMap` / `Option`. |
| `L2`  | error    | Lean | Bare closure-as-value or unsupported fold accumulator (e.g. in-memory `HashMap`). |
| `L5`  | error    | Lean | `expect throw N` targets a total route (no `where` / `throw` / unrescued extern CALL) — theorem would be vacuous. |
| `L6`  | error    | Lean | `expect return …` targets a route with no `return_type`. |
| `L7`  | warning  | Lean | `skip from` is not yet honoured on Lean (`from`-clauses still gate). |
| `L8`  | error    | Lean-EVM pair | Typed send dest does not statically resolve to an in-program `Entity.address(...)` / `addressOf`. |
| `L9`  | error    | Lean-EVM pair | Capturing self-call to a failing route without a fail surface (`where`/`throw`). |
| `L10` | warning  | Lean-EVM pair | Invariant action route contains a send / typed self-call (atomic-step elides interleavings). |
| `L11` | error    | Lean-EVM pair | Fire-and-forget self-send to a failing route without a fail surface. |
| `L13` | error    | Lean | `std::crypto::*` (and similar) has no Lean lowering. |
| `L14` | error    | Lean | Bare type identifier does not resolve to a primitive, record, enum, or alias visible from Lean (would emit an opaque identifier and only fail at `lake build`). |
| `L15` | error    | Lean | HashMap member-transform body shape is unrecognised (would lower to an ill-typed or silently wrong member def). Use `m.insert(k, v)` / `m.update(k, v)` / `m.remove(k)` (optionally inside `if`, `block`, or `let`). Mirror of Solidity `E17`. |
| `L16` | error    | Lean | Namespaced call (`std::…` / `evm::…` / other) with this name/arity has no Lean lowering (would emit `Cambrian.Unsupported` into a value slot). `std::crypto::*` stays **L13**. |

### T — `test` block rules

Full discussion in [§ Testing](#testing). Quick reference:

| Code  | Severity | Binding | Meaning |
|-------|----------|---------|---------|
| `T1`  | error    | Universal | `test "..." for Entity` (or `invariant` / `fuzz` `for`) references an unknown entity / instance. |
| `T2`  | error    | Universal | `call <route>(...)` references an unknown route on the entity. |
| `T3`  | error    | Universal | `call <route>(...)` arity does not match the route signature. |
| `T4`  | error    | Universal | `with { field: ... }` / `expect state { field: ... }` references a non-member field. |
| `T5`  | error    | Universal | `expect <kind>` appears before any `call`. |
| `T6`  | error    | Universal | `test "..."` body contains no `call`. |
| `T8`  | warning  | Universal | `<ns> { ... }` context block uses an unknown namespace (known: `msg`, `sys`). |
| `T10` | error    | Universal | `property` parameter name collides with an entity member, or a `let` shadows a parameter. |
| `T11` | error    | Universal | `bound` used in a property body (belongs in a `fuzz` instance); **or** an instance binding references an undeclared / duplicated parameter; **or** `bound`/`assume` used inside a regular `test` block. |
| `T12` | error    | Universal | `assume <expr>` body must be a boolean expression; **or** used inside a regular `test` block. |
| `T13` | —        | — (folded into `W7`) | *Not emitted as a separate code.* Property parameter types unsupported by the selected backend produce a `W7` fallback-strategy warning instead of a hard error. |
| `T15` | error    | Universal | `skip if` used outside an invariant `action { ... }` body. |
| `T16` | error    | Universal | `advanceTime(...)` used outside an invariant action body (requires `#[with_time]` on the invariant). |
| `T17` | error    | Universal | Instance binding form mismatch — `test` requires `p: value`, `fuzz` requires `p in lo..hi`. |
| `T18` | error    | Universal | A `test` instance must bind every property parameter to a concrete value. |
| `T19` | error    | Universal | Forall marker `field: *` in `with { ... }` references a field that is not an entity member (properties and invariants). |
| `T20` | error    | Universal | Context parameter `msg::x` / `sys::x` in a `ctx { ... }` block uses an unknown name (allowed: `msg::{sender,value}`, `sys::{now,timestamp,chainid,block_number,balance}`). |
| `T21` | error    | Universal | A field is both pinned (`field: v`) and forall-ized (`field: *`) in the same `with { ... }`. |
| `T22` | error    | Universal | Context parameter (`msg::x` / `sys::x`) declared in `with { ... }` / `init { ... }`; context belongs in a dedicated `ctx { ... }` block. |
| `T23` | error    | Universal | Context parameter declared more than once in the same `ctx { ... }` block. |
| `T38` | error    | Universal | Boolean `expect <expr>` is not boolean-shaped (tuple, record, array, or empty collection); **or** it mentions `result` but the preceding route has no return type; **or** it mentions `result` when a parameter, `let`, or member of that name is in scope. `expect` before any `call` is still `T5`. |
| `T39` | error    | Solidity | `expect state` on a member with no public Solidity getter (`Vec`, a record holding a `Vec` / `HashMap`, or a map to `Vec`). |

### I — `invariant` block rules

Full discussion in [§ Stateful invariant tests](#stateful-invariant-tests).
Cross-link table preserved unchanged here for completeness.

| Code  | Severity | Binding | Meaning |
|-------|----------|---------|---------|
| `I1`  | error    | Universal | `action <name>` references an unknown route on the entity. |
| `I2`  | error    | Universal | `action` parameter list does not match the target route's signature (arity / types). |
| `I3`  | error    | Universal | `check` expression is not boolean-shaped. |
| `I4`  | error    | Universal | Action body contains a forbidden statement (`call`, `expect*`, `let`); only `bound` / `assume` / `skip if` / `advanceTime(...)` are allowed. |
| `I5`  | error    | Universal | Invariant must declare at least one `action` and at least one `check`. |
| `I6`  | error    | Universal | `senders { ... }` entry is not an address/pubkey literal or constant identifier. |
| `I8`  | error    | Universal | Duplicate instance name within an invariant. |
| `I9`  | error    | Universal | Instance entity does not exist, or `action <inst>.<route>` references an undeclared instance. |
| `I10` | error    | Universal | Per-instance `init <inst> { field: ... }` references a non-member field. |
| `I11` | error    | Universal | In a multi-entity invariant, member references in `check`/`bound`/`assume` must be qualified `<inst>.<member>`. |
| `I12` | error    | Universal | `derived` query body may only contain `let` bindings (queries are pure view helpers). |
| `I13` | error    | Universal | Duplicate `track { let name = expr; ... }` binding within an invariant. |
| `I14` | error    | Universal | `exclude selectors { ... }` entry does not match a declared action route. |
| `I15` | error    | Universal | `trace::count(route)` / `trace::lastWas(route)` route is not a declared action, or the accessor name is not `length`/`count`/`lastWas`. |
| `I16` | error    | Universal | `trace::*` accessor used outside an invariant `assume`/`check` (e.g. in `bound`/`skip if`/`derived`/`track`, a route, property, or test). |
| `I17` | error    | Solidity | Invariant `check` cannot be lowered on EVM (Foundry). Point at library/`keys().fold` or rewrite as a `view` route. `trace::*` is Inline (handler counters), not I17. Lean is exempt. |
| `I18` | error    | Domain EVM | Multi-instance invariant: two or more instances of the same entity without `identity` members, a duplicate identity tuple across instances, or a cyclic instance deploy-order graph. |

### F — project-config rules

Mirror of [§ Project-config validation rules](#project-config-validation-rules).

| Code  | Severity | Binding | Meaning |
|-------|----------|---------|---------|
| `F1`  | error    | Packaging | `*.coverage.enabled` requires the matching backend to be enabled. |
| `F2`  | error    | Packaging | Unknown `lean.numerics` (expected `nat` / `overflow-wrap` / `overflow-panic`). |
| `F3`  | error    | Packaging | `*.coverage.engine` must be one of `libfuzzer`, `afl`. |
| `F4`  | error    | Packaging | Imported file declares `entity` / `test` / `fuzz` / `invariant`. |
| `F5`  | error    | Packaging | `imports:` or a bare `import "…"` did not resolve against `library_paths`. |
| `F6`  | error    | Packaging | `deterministic_addresses: false` on `target: evm` (CREATE2 factory deploy is mandatory). |
| `F7`  | warning  | Packaging | Unknown `project.yaml` key (ignored), or `fuzz.shrink` (no effect). |

### W — non-fatal lints / warnings

| Code  | Severity | Binding | Meaning |
|-------|----------|---------|---------|
| `W1`  | warning  | Universal | Member has no transforms (dead state?). |
| `W2`  | warning  | Universal | Pure function is never called from any entity (or transitively from another reachable pure fn). |
| `W3`  | warning  | Universal | Constant defined on the entity is never referenced. |
| `W4`  | warning  | Universal | Duplicate `use <ns>` import. |
| `W5`  | warning  | Universal | `deploy <Entity>(...)` references an entity not defined in this program. |
| `W6`  | warning  | Universal | Deprecated TVM code-upgrade effect (`gosh::setcode` and similar); not applicable on EVM / Lean. |
| `W7`  | warning  | Targeted | Fuzz parameter has a type that the selected target's fuzz backend lowers via a fallback strategy (needs a concrete target to name the backend). |
| `W8`  | warning  | Universal | A `test` instance leaves a property-level forall **state** field (`m_count: *`) unpinned, so it falls back to the member default (a concrete `test` cannot quantify). |
| `W9`  | warning  | Universal | Entity-local `record` / `enum` / `type` shares a name with a program-scope declaration. The two are distinct types (Lean will not coerce them); lift one declaration or rename. |
| `W10` | warning  | Domain EVM | Route updates a balance / allowance member but has no `emit`; member transforms do not log. |
| `W11` | warning  | Universal | Test calls the init route and only then sets `msg { sender: … }`; the init route ran under the previous context. Put `msg` first. |
| `W13` | warning  | Universal | Invariant constructor bootstrap without an explicit `deploy { ... }` uses legacy harness semantics (informational). |
| `W14` | warning  | Universal | Invariant deploy address is also listed under `senders` (informational). |
| `W15` | warning  | Universal | Invariant `check` pins a member / view to a constant with `==` while a non-excluded `action` can decrease it (vacuous under fuzz). |
