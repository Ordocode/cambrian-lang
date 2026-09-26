# Cambrian EVM Target — Gap Inventory

> This document is the **gap inventory** (what is missing today, with a
> Solidity / EVM reference for each item). Use it to decide whether the
> EVM target can express a given contract today, and to feed future
> planning sessions. Phase labels like `PLAN_EVM-N` / `PLAN_EVM-P0-X`
> refer to the internal phase tracker where that work was scheduled.

## How to read this document

Each entry has the same shape:

```
### <Feature name>
- Solidity reference: <doc anchor / EIP>
- Cambrian status: yes | partial | no
- Codegen site: <file:line> or "missing"
- Priority: P0 | P1 | P2
- Workaround today: <one line, or "none">
- Scheduled: <PLAN_EVM phase> | unscheduled
```

Priority is informational. It does not commit any work.


| Code   | Meaning                                                                                                                                                                                                                 |
| ------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **P0** | Remaining blockers for real-world EVM contracts: `assembly` escape hatch, `library`/`using for`, multi-file imports, ERC-165 / EIP-1167 / EIP-2535, modifiers, `virtual`/`override`. Events, custom errors, `fallback`/`receive`, `unchecked { }`, and `payable` constructor are **shipped** (EVM-P0 / EVM-15). |
| **P1** | Needed for ecosystem parity: `assembly` escape hatch, `library` / `using for`, multi-file imports, ERC-165 / EIP-1167 / EIP-2535, modifiers, `virtual` / `override`, full transient-storage support.                    |
| **P2** | Niche, deferred, or by-design TVM↔EVM semantic mismatch (`gosh::`* no-ops, `msg::pubkey`, async-bounce vs sync-revert). Tagged so future planning sessions don't re-discover them.                                      |


Sections:

1. [Codegen fallbacks in the EVM backend](#1-codegen-fallbacks-in-the-evm-backend) — every `revert(...)` stub, no-op comment, or silent default in the lowering pipeline (`solidity/core/expr.rs`, `solidity/core/iter.rs`, `solidity/evm/emitter.rs`, `solidity/evm/route.rs`, etc.).
2. [Solidity features missing from the Cambrian surface](#2-solidity-features-missing-from-the-cambrian-surface) — features that are not reachable today because the surface syntax doesn't exist.
3. [By-design TVM↔EVM mismatches](#3-by-design-tvmevm-mismatches) — re-citations of the internal tracker's Semantic Gap Summary for completeness.
4. [Cross-target test gaps](#4-cross-target-test-gaps) — sites where `test`/`fuzz`/`invariant` lowering silently skips on EVM.
5. [Roadmap pointers](#5-roadmap-pointers) — one table mapping every gap to a `PLAN_EVM-N` phase or `unscheduled`.

### EVM codegen layout (module map)

The EVM backend no longer uses a monolithic `codegen/evm.rs`. Lowering lives under
`cambrian-transpiler/src/codegen/solidity/`:

| Area | Path | Examples |
| --- | --- | --- |
| **SolidityCore** | `solidity/core/` | `types.rs` (`infer_let_type_entity`, `InferSolMode`, `actual_sol_type`), `expr.rs` (`gen_expr_hoisted`), `iter.rs` (`gen_for_loop`, `gen_fold_loop`, `lower_iter_chain`), `pure.rs`, `ctx.rs` (`EmitScope`) |
| **EvmSolidity** | `solidity/evm/` | `route.rs`, `emitter.rs` (`EvmActionEmitter`), `entity.rs`, `factory.rs`, `transform.rs` (`gen_mapping_transform_split`) |
| **Harness** | `evm_test_codegen.rs`, `evm_revm_test_codegen.rs` | Foundry / revm test emission |

Type-inference hardening (audit 2026-09): [docs/plans/evm-codegen-type-inference.md](plans/evm-codegen-type-inference.md).

Older gap entries may still cite `solidity/` or stale line numbers — use the module map above.

For language reference, this document refers to:

- Solidity 0.8.x reference — `https://docs.soliditylang.org/en/v0.8.24/`
- EVM Yellow Paper (Cancun revision)
- EIPs: [EIP-712](https://eips.ethereum.org/EIPS/eip-712) (typed data), [EIP-191](https://eips.ethereum.org/EIPS/eip-191) (signed data), [EIP-1153](https://eips.ethereum.org/EIPS/eip-1153) (transient storage), [EIP-1167](https://eips.ethereum.org/EIPS/eip-1167) (minimal proxies), [EIP-1822](https://eips.ethereum.org/EIPS/eip-1822) / [EIP-1967](https://eips.ethereum.org/EIPS/eip-1967) (proxies), [EIP-2535](https://eips.ethereum.org/EIPS/eip-2535) (diamonds), [EIP-2612](https://eips.ethereum.org/EIPS/eip-2612) (permit), [EIP-4626](https://eips.ethereum.org/EIPS/eip-4626) (tokenised vaults).

---

## 1. Codegen fallbacks in the EVM backend

These are sites where the Cambrian source compiles, but the generated
Solidity is either a `revert(...)` stub, a no-op comment, or a typed
default (`uint256`, `address(0)`, `bytes`) inserted to keep `solc`
happy. Each of them is a real expressivity gap — a contract that
relies on the construct will compile but fail at runtime, or behave
silently wrong.

### 1.1 `Expr::EnumVariantWithData` lowering — DONE (Phase EVM-4 J1-J3)

- Solidity reference: [enums](https://docs.soliditylang.org/en/v0.8.24/types.html#enums) (Solidity enums are unit-only).
- Cambrian status: **DONE**. Payload-bearing Cambrian enums lower to a Solidity tagged-union pair: `enum <N>_Tag { ... }` plus `struct <N> { <N>_Tag tag; ...payload-fields-flat... }`. Constructor calls (`Action::Deposit(amount)`) emit a struct literal with the active variant's fields populated and every other variant's payload slot zero-initialised via `solidity_default_for_member_ty`. Match arms dispatch on `subj.tag == <N>_Tag.<V>` and rewrite payload bindings to `subj.<variant>_<index>` field accesses. See § 1.31 for the storage trade-off.
- Tests: `tests/test_codegen_evm.rs::evm4_j*_`* plus the `enum_data` fixture in `tests/test_evm_solc.rs::ALL_FIXTURES` (graduation check).
- Workaround: not needed.

### 1.2 `Expr::NamespacedCall` for non-`evm` namespaces in expression position — DONE (Phase EVM-2 K1)

- Cambrian status: **DONE**. Validator rule `E15` rejects `gosh::`* in expression position with a focused error pointing users to action position (which still lowers to a no-op + `E02` warning, which is harmless). `E16` covers every other non-`evm` namespace with the same treatment. The codegen catch-all in `gen_expr_hoisted` is a `debug_assert!(false, ...)` plus a self-documenting comment marker.
- Tests: `tests/test_validate.rs::evm2_e15_gosh_namespaced_call_in_expr_position_errors` and `evm2_e15_does_not_fire_for_evm_namespace`.
- Workaround: not needed.

### 1.3 `Expr::Range` outside `for` / `.fold` iterator position

- Solidity reference: n/a (Solidity has no range type).
- Cambrian status: **partial** — supported as an iterator for `for` and `.fold`; bare ranges as values revert at runtime.
- Codegen site: `gen_expr_hoisted` in [`solidity/core/expr.rs`](../cambrian-transpiler/src/codegen/solidity/core/expr.rs) (`Expr::Range` arm).
- Priority: P2 — current uses are loop iterators only.
- Workaround: keep `Range` strictly as the iterator argument to `for` or `.fold`.
- Scheduled: unscheduled.

### 1.4 `Expr::Closure` as a first-class value

- Solidity reference: [function types](https://docs.soliditylang.org/en/v0.8.24/types.html#function-types) (Solidity has function pointers, but Cambrian closures don't lower to them).
- Cambrian status: **partial** — closures work as the second argument of `.fold`; bare closures-as-values revert.
- Codegen site: `gen_expr_hoisted` in [`solidity/core/expr.rs`](../cambrian-transpiler/src/codegen/solidity/core/expr.rs) (`Expr::Closure` arm).
- Priority: P2
- Workaround: rewrite around an explicit `for` / direct `.fold`, or keep the algorithm in a TVM-only `pure fn`.
- Scheduled: `PLAN_EVM-12` item 2 (iterator-method-chain desugaring) — chain fusion covers most cases; bare closure-as-value still reverts.

### 1.5 `gen_expr_hoisted` final `_` arm — DONE (Phase EVM-2 K1+K3)

- Solidity reference: n/a.
- Cambrian status: **DONE**. The validator's `E16` rule (`check_evm_compat_expr`) rejects every `Expr` variant that has no defined EVM lowering (`Expr::Encode`, `Expr::AddressOf` outside deterministic mode, `Expr::NamespacedCall` for non-`evm`/non-`gosh` namespaces, etc.). The codegen catch-all in `gen_expr_hoisted` is now a `debug_assert!(false, "EVM-2 K3: ...")` plus a self-documenting `// EVM-2 K3:` marker — debug builds panic, release builds emit a comment that downstream `solc` will surface as a missing identifier rather than silently miscomputed value.
- Tests: `tests/test_validate.rs::evm2_e16_address_of_outside_deterministic_mode_errors` and the EVM-15 H1 graduation check pin the behaviour.
- Workaround: not needed.

### 1.6 `gen_expr` final `_ => None` — DONE (Phase EVM-2 K1+K3)

- Cambrian status: **DONE**. Validator `E16` covers expression-position uses; `gen_expr` returning `None` is now reserved for compositional callers (e.g. `gen_match_simple` falling through to the hoisted match, `gen_expr_hoisted`'s `Expr::FnCall` / `Expr::MethodCall` arms recursing with proper arg hoisting). Anything that escapes both paths trips the `debug_assert!` in the hoisted final arm — see § 1.5 above.
- Companion validator rules: `E15` (`gosh::*` in expr position), `E17` (HashMap member-transform shape), `E18`-`E21` (silent type erasure — see §§ 1.20-1.22, 1.24).
- Workaround: not needed.

### 1.7 `for` non-`Ident` / non-`Wildcard` pattern

- Solidity reference: [`for` statement](https://docs.soliditylang.org/en/v0.8.24/control-structures.html#for-loop).
- Cambrian status: **DONE** (Batch A) — `for x in ...`, `for _ in ...`, `for (k, v) in m.iter() { … }`, `for (i, x) in xs.enumerate() { … }`, and `for (a, b, …) in vec_of_tuples { … }` all lower via the generic `bind_loop_pattern` helper. Each tuple component becomes its own per-iteration Solidity local; wildcard slots are skipped silently.
- Codegen site: `bind_loop_pattern` and `gen_for_loop` in [`solidity/core/iter.rs`](../cambrian-transpiler/src/codegen/solidity/core/iter.rs).
- Workaround: not needed.
- Scheduled: `PLAN_EVM-12` item 3 — DONE.

### 1.8 `.fold` non-`Ident` / non-`Wildcard` accumulator or loop variable

- Cambrian status: **DONE** (Batches A & C) — the *loop-variable* pattern accepts arbitrary tuple shapes via `bind_loop_pattern` (Batch A); the *accumulator* now supports record types (`MyRec memory acc = init;`) seeded with `infer_fold_acc_ty` (which falls back to the closure body's tail when the init alone defaults to `uint256`, e.g. when init is an `Expr::Ident` for a let-bound record local). Acc destructure patterns (`Pattern::Tuple` / hypothetical `Pattern::Record`) remain rejected with a focused diagnostic pointing at field-access (`acc.field`) as the alternative. HashMap accumulators are rejected via `is_hashmap_acc_init` with an actionable workaround (use a per-entity storage staging map).
- Codegen site: `gen_fold_loop`, `lower_iter_chain`, `infer_fold_acc_ty`, and `is_hashmap_acc_init` in [`solidity/core/iter.rs`](../cambrian-transpiler/src/codegen/solidity/core/iter.rs).
- Workaround: not needed; refer to the staging-map idiom for HashMap accs.
- Scheduled: `PLAN_EVM-12` item 4 — DONE.

### 1.9 Iterator chain prefixes (`.iter()` / `.enumerate()` / `.filter()` / `.map()` / `.take()`) and action-level `for`

- Solidity reference: n/a — Solidity has no first-class higher-order iterator API, so the EVM backend fuses chains at codegen time.
- Cambrian status: **DONE** (Batches A–E) — Phase EVM-12 closed.
  - **Chain fusion** (Batch B): `<src>.{iter|enumerate|filter|map|take}*.{fold|collect}` chains over `Range`, `Vec<T>`, and HashMap members lower to a single Solidity `for`-loop in `lower_iter_chain` ([`solidity/core/iter.rs`](../cambrian-transpiler/src/codegen/solidity/core/iter.rs)). `.iter()` is a pass-through (Vec) or routes to the parallel-keys walk (HashMap); `.enumerate()` synthesises an `(idx, elem)` tuple; `.filter` / `.map` / `.take` lower to inline `continue` / rebind / `break`. `.collect()` materialises a sized `T[] memory` pre-allocated at source length and trimmed via `assembly { mstore(arr, len) }`. **2026-09:** enumerate+filter+map collect uses `InferSolMode` + `EmitScope.expected_ty` hints (see [evm-codegen-type-inference.md](plans/evm-codegen-type-inference.md)).
  - **Tuple destructuring** (Batch A): `bind_loop_pattern` decomposes `Pattern::Tuple` into per-component Solidity locals; shared between `gen_for_loop`, `gen_fold_loop`, the chain emitter, and the action-level `for` emitter.
  - **Non-scalar `.fold` accumulators** (Batch C): record and tuple accumulators are supported; in-memory `HashMap` accumulators stay rejected by design with a focused diagnostic that points at the storage-map workaround.
  - **Action-level `for`** (Batch E): new grammar `for <pat> in <iter> => [ <actions> ]` parses to `RouteAction::For { pattern, iter, body }` and lowers to a per-iteration loop on EVM (Solidity `for`) and Acki Nacki (Rust `for` in the WASM body). Iterator sources cover route-parameter `Vec<T>`, state-member `Vec<T>` / `HashMap<K, V>` (with `for (k, v) in m` sugar), and ranges. Validator rule **V29** restricts the body to effects only — `var x = msg(args) ~> dest`, `return(...)`, and `gosh::updateCode(...)` are rejected because none of them have safe per-iteration semantics. Fixture: `contracts/airdrop_evm.cam`.
- The only construct that still falls back to a `revert(...)` stub is a bare closure-as-value used outside a recognised chain (§ 1.4); that requires either a separate first-class function lowering or a TVM-only `pure fn`.
- Workaround: not needed.
- Scheduled: `PLAN_EVM-12` — DONE.

### 1.10 Mapping transform "no-op" comment when shape isn't recognised

- Solidity reference: [mappings](https://docs.soliditylang.org/en/v0.8.24/types.html#mapping-types).
- Codegen site: `gen_mapping_transform_split` in [`solidity/evm/transform.rs`](../cambrian-transpiler/src/codegen/solidity/evm/transform.rs).
- Priority: P0 — silent no-op for an unrecognised mapping update is dangerous.
- Cambrian status: **DONE** (Phase EVM-2 K1+K3). Validator rule `E17` (`check_evm_compat_member_transforms` + `is_recognised_hashmap_transform`) rejects any HashMap member-transform body whose top level is not `m.insert(...)` / `m.update(...)` / `m.remove(...)` (optionally wrapped in `if` / `block` / `let` / `EmptyCollection` / `HashMap::new()` / bare `m`). The codegen no-op fallback is now a `debug_assert!(false, "EVM-2 K3: ...")` plus a self-documenting comment marker.
- Tests: `tests/test_validate.rs::evm2_e17_unrecognized_mapping_transform_shape_errors` and `evm2_e17_recognised_insert_transform_ok`.
- Workaround: not needed — invalid shapes are now rejected at validation.

### 1.11 `EvmActionEmitter::emit_let` — non-`Ident`/non-`Tuple` patterns revert

- Solidity reference: variable declaration and tuple destructure are first-class; `Some` / `Deref` / `None` are not.
- Cambrian status: **partial** — `let x = ...` and `let (a, b, c) = ...` (uint256-only slots) work; `let some(x) = ...`, `let *x = ...`, `let none = ...` revert at runtime.
- Codegen site: `EvmActionEmitter::emit_let` in [`solidity/evm/emitter.rs`](../cambrian-transpiler/src/codegen/solidity/evm/emitter.rs).
- Priority: P1 — relevant once option / record destructuring becomes idiomatic on EVM.
- Workaround: name the option as a value and inspect via `if`.
- Scheduled: unscheduled.

### 1.12 `let (a, b, c) = ...` mixed-type tuple destructure

- Solidity reference: [tuple assignment](https://docs.soliditylang.org/en/v0.8.24/control-structures.html#destructuring-assignments-and-returning-multiple-values).
- Cambrian status: **done** — destructured slots are now typed per-element via `infer_tuple_elem_types`, so `(a, b, c)` from a `(address, uint256, bool)`-returning pure fn or `~>` capture gets the right Solidity types. Validator E23 backstops the cases where per-slot inference cannot prove a type, surfacing the issue at validation rather than emitting a wrong-typed `uint256` slot.
- Priority: ~~P0~~ DONE.
- Workaround: none needed.
- Scheduled: shipped in PLAN_EVM-P0-A.

### 1.13 `RouteAction::Effect` (`gosh::`*) → no-op comment

- Codegen site: `EvmActionEmitter::emit_effect` in [`solidity/evm/emitter.rs`](../cambrian-transpiler/src/codegen/solidity/evm/emitter.rs).
- Priority: P2 (by design — see § 3).
- Cambrian status: **resolved** — the codegen emits a `// EVM: gosh::name(args) — no-op on EVM` comment, and the validator now warns (E01/E02/E06) under PLAN_EVM-9 so users get a heads-up before deploying.
- Scheduled: **done** (PLAN_EVM-9).


### 1.14 `RouteAction::UpdateCode` → trait-default no-op

- Codegen site: `[cambrian-transpiler/src/codegen/adapter.rs:97-102](../cambrian-transpiler/src/codegen/adapter.rs)` (trait default; `EvmActionEmitter` does not override).
- Priority: P2 (by design — EVM has no in-place code replacement).
- Workaround: use a proxy pattern (UUPS / transparent / beacon) — none of which Cambrian generates today.
- Scheduled: unscheduled. Could be folded into a future "EVM upgradeability" phase.

```97:102:cambrian-transpiler/src/codegen/adapter.rs
    fn emit_update_code(
        &self, _update_args: &[Expr], _callback_route: &str, _callback_args: &[Expr],
        _entity: &Entity, _route: &Route, _program: &Program, indent: &str,
    ) -> String {
        format!("{}// updateCode not supported on this target\n", indent)
    }
```

### 1.15 `rescue` / `recover` — Acki Nacki-only (E26)

- Cambrian status: **rejected on EVM** (2026-08-20, PN-105 WONT FIX). `rescue`/`recover` is TVM async bounce recovery. The former `try`/`catch` analogue was withdrawn. There is **no contained-failure / try-catch mechanism on EVM yet**; `where` / `throw` abort the route.
- Tag pairing (`rescue` ↔ `recover`) remains a **universal** V8 check and still applies on Acki Nacki.
- See [docs/plans/pn-105-rescue-evm-reject.md](plans/pn-105-rescue-evm-reject.md).

### 1.16 `evm::*` namespace — only two entries wired

- Solidity reference: see [global functions](https://docs.soliditylang.org/en/v0.8.24/units-and-global-variables.html#mathematical-and-cryptographic-functions).
- Cambrian status: **partial** — `evm::ecrecover`, `evm::keccak256Packed`, `evm::sha256`, `evm::ripemd160`, `evm::balance`, and `evm::blockhash` are recognised; the remaining entries (`evm::create2`, `evm::selfdestruct`, `evm::staticcall`, `evm::delegatecall`, …) still return `None` and fall through to the silent `/* unsupported expr */ 0` (see 1.5).
- Codegen site: `gen_evm_ns` in [`solidity/core/expr.rs`](../cambrian-transpiler/src/codegen/solidity/core/expr.rs).
- Priority: P0 → P1 — the most common cryptographic / context reads are now wired; the long tail (`create2`, low-level calls, `selfdestruct`) is still missing.
- Workaround: stick to `hashOf(...)` / the wired intrinsics for now.
- Scheduled: hash + balance + blockhash arms **done in batch 1** (see Phase EVM-14). Remaining arms unscheduled.

### 1.17 `MatchPattern::EnumVariantWithData` in `match` arms — DONE (Phase EVM-4 J3)

- Cambrian status: **DONE**. Both `gen_match_simple` (inline ternary) and `gen_expr_hoisted`'s `Expr::Match` arm now lower `MatchPattern::EnumVariantWithData(en, vn, payload_pats)` to `subj.tag == <En>_Tag.<Vn>` (via `payload_or_unit_enum_cond`) and substitute every binder with `Expr::FieldAccess(Expr::Ident(subj_str), <vn>_<i>)` via `subst_payload_bindings` before the body lowers. Wildcards (`_`) and literal patterns are also accepted; only the active binders are rewritten.
- Tests: `tests/test_codegen_evm.rs::evm4_j3_match_payload_enum_unpacks_bindings_via_field_access` and `evm4_j3_match_wildcard_payload_pattern_emits_no_binding`. The `enum_data` fixture exercises this end-to-end through the EVM-15 H1 graduation check.
- Workaround: not needed.

### 1.18 `Expr::Match` with non-simple arm bodies in inline position — DONE (Phase EVM-4 J3 + by design)

- Cambrian status: **DONE**. `gen_match_simple` now recognises `EnumVariantWithData` (via the same path as 1.17). The remaining `_ => return None` arms cover patterns whose bodies legitimately need the full statement-level hoisted form (e.g. nested matches with sequencing); falling through to `gen_expr_hoisted` is the intended path and produces semantically identical Solidity. No miscompile.
- Workaround: not needed.

### 1.19 `MethodCall::exists` lowering — DONE (Phase EVM-3 Batch G)

- Solidity reference: mappings have no membership test; `exists` requires a sidecar `mapping(K => bool)`.
- Cambrian status: **DONE** — covered uniformly across the shapes that actually arise in idiomatic Cambrian. Specifically:
  - `m.exists(k)` on an entity-member HashMap → `m_exists[k]`, with the `mapping(K => bool) public <m>_exists` companion emitted whenever **any** reachable expression in the entity calls `.exists()` on the member (route bodies, route `where` clauses, return values, member transforms, member defaults, and pure-fn arguments — see `member_uses_exists` in [`solidity/evm/analysis.rs`](../cambrian-transpiler/src/codegen/solidity/evm/analysis.rs)).
  - `m[k1].exists(k2)` for `m: HashMap<K1, HashMap<K2, V>>` → `m_inner_exists[k1][k2]`, with the 2-level companion `mapping(K1 => mapping(K2 => bool)) public <m>_inner_exists` emitted when `member_inner_uses_exists` fires. Maintenance for nested writes (`m[k1][k2] = v;`) is paired with a `m_inner_exists[k1][k2] = true;` line so subsequent reads stay consistent.
  - `let inner = m[k1]; … inner.exists(k2)` and the `let inner = if m.exists(k1) { m[k1] } else { {} }; …` boilerplate are recognised by `is_hashmap_valued_expr` + `subst_ident_in_expr` and substituted at codegen time — Solidity disallows mapping locals, so the `let` produces no Solidity local; uses of `inner` are inlined to the underlying `m[k1]`. The triple-subscript bug in `gen_mapping_transform_split` (which was producing `m[k1][k1][k2] = v;`) was fixed in the same pass.
  - `pure fn f(m: HashMap<K, V>, …)` calling `.exists()` on `m` now gains a parallel `mapping(K => bool) storage <param>_exists` parameter; every call site `f(m_balances, …)` is rewritten to thread the matching `m_balances_exists` sidecar automatically. Previously the body referenced an undeclared `<param>_exists` symbol and solc rejected the function.
- Codegen sites: `gen_expr` (`exists` arm), `gen_mapping_transform_split` (HashMap-let substitution + nested-write maintenance), `gen_pure_fn` + `Expr::FnCall` (sidecar parameter threading), `member_uses_exists` / `member_inner_uses_exists` (detection).
- Tests: `evm3_g1_exists_in_where_clause_emits_sidecar`, `evm3_g1_exists_via_pure_fn_threads_sidecar_argument`, `evm3_g2_let_bound_hashmap_alias_is_inlined`, `evm3_g2_nested_exists_emits_two_level_sidecar`, `evm3_g2_nested_update_emits_single_subscript_pair_and_sidecar_write` in `[test_codegen_evm.rs](../cambrian-transpiler/tests/test_codegen_evm.rs)`.
- Out of scope (and unrelated to `.exists()`): the four large fixtures `token`, `nft`, `dex`, `staking` no longer hit any `.exists()`-related solc errors after Batch G; the *separate* string/struct memory-annotation gap that still kept them on the `evm10_all_fixtures_compile_with_solc` skip list was closed in Phase EVM-15 H2 (see § 1.28 below).

### 1.20 `Type::Tuple` erased to `bytes` in `sol_type` — DONE (Phase EVM-2 K2)

- Solidity reference: tuples are not first-class types in Solidity (only as expressions / multi-returns).
- Cambrian status: **DONE**. Validator rule `E18` (`check_evm_compat_type_pos`) rejects `Type::Tuple` everywhere except multi-return position (route / pure-fn / macro return types), where Solidity legitimately splits them into multiple named return values.
- Tests: `tests/test_validate.rs::evm2_e18_tuple_storage_member_errors` and `evm2_e18_tuple_in_return_type_is_allowed`.
- Workaround: model tuple state as a named `record` (which lowers to a Solidity `struct`).

### 1.21 Unknown `Type::Generic` shape erased to `bytes` — DONE (Phase EVM-2 K2)

- Cambrian status: **DONE**. Validator rule `E19` rejects every `Type::Generic(name, ...)` whose name is not in `{Vec, HashMap, Option}` on the EVM target.
- Tests: `tests/test_validate.rs::evm2_e19_unknown_generic_errors` and `evm2_e19_known_generics_allowed`.
- Workaround: stick to the three supported generics.

### 1.22 Unknown `Type::Simple` (custom alias / unknown name) → `uint256` — DONE (Phase EVM-2 K2)

- Cambrian status: **DONE**. Validator rule `E20` rejects any `Type::Simple(name)` that does not resolve to a primitive (Cambrian or Solidity-native), entity / program record / enum, type alias, or known entity name on the EVM target. Type aliases are resolved through `program.type_aliases` and `entity.type_aliases`.
- Tests: `tests/test_validate.rs::evm2_e20_unknown_simple_type_errors` and `evm2_e20_type_alias_resolves_through_program`.
- Workaround: not needed.

### 1.23 `for` element type defaults to `uint256` when iterator type cannot be inferred

- Codegen site: [`solidity/core/iter.rs`](../cambrian-transpiler/src/codegen/solidity/core/iter.rs) (`gen_for_loop`, `gen_fold_loop`, `infer_range_elem_ty`).
- Priority: P1 — `Ident` (entity member), `FnCall` (known pure fn), `Index` (`HashMap<K, Vec<T>>` member), and `FieldAccess` (record-field of a `Vec<T>`) are now inferred; iterators over `var` captures from previous phases and arbitrary `MethodCall`s still fall through to the `uint256` default.
- Workaround: stage the `Vec<T>` into a local `let` whose type can be inferred, or hoist into an entity member / record field.
- Scheduled: bare-Ident + Index + FieldAccess arms **done in batch 1** (see Phase EVM-14). Remaining cases unscheduled.

### 1.24 `Expr::Cast` to non-primitive type silently drops the cast — DONE (Phase EVM-2 K2)

- Cambrian status: **DONE**. Validator rule `E21` (`check_evm_compat_cast_target`) rejects `as T` whenever `T` resolves (through `resolve_alias_for_validate`) to anything other than the cast-allowlist `{uint256, int256, bool, address, bytes32}`. Type aliases pointing at a primitive (e.g. `type Amount = U256`) are accepted.
- Tests: `tests/test_validate.rs::evm2_e21_non_primitive_cast_errors` and `evm2_e21_alias_to_primitive_cast_ok`.
- Workaround: not needed.

### 1.25 `infer_let_type_entity` defaults

- Codegen site: `cambrian-transpiler/src/codegen/solidity/core/types.rs` (`infer_let_type_entity`, `actual_sol_type`, `InferSolMode`). Recognised shapes include literals, idents (let-bindings + entity members), `MethodCall`s, `Cast`, `If`/`Match`/`Block`, `RecordConstruct`, pure-fn returns, `Range` (narrow bounds via `infer_range_elem_ty` + `Vec<T>` hint), iterator chains (enumerate filter/map bindings), `Some(inner)`, `EnumVariant`, and `FieldAccess` on entity-record members.
- **2026-09-16 (audit Phase B–D):** `EmitScope.expected_ty` threads pure-fn / route-return hints into fold, scalar-reduce, binop narrowing, and `.collect()` element allocation. Dual infer paths remain (`InferSolMode::Decl` vs `Flow`) with shared `infer_binop_sol_ty` for binops.
- Priority: P1 — narrowed but not eliminated. Unbound `Ident` (TI-07), nested `Index` (TI-16), and exotic `MethodCall` chains still default to `uint256`.
- Workaround: keep `let` RHS shapes in the recognised set; use typed members / `let` bindings before range bounds; prefer tuple-destructure on `.enumerate()` chains.
- Scheduled: Phase C consolidation **done** @ `c7a4f845` (`InferSolMode`, `infer_expr_sol_ty`, shared binop/cast/ident helpers); TI-16 nested index on demand.

### 1.26 `from Entity(args)` with `args.len() != 1` → `false` — DONE (Phase EVM-6 M2)

- Solidity reference: this was a Cambrian-side codegen fall-through; nothing in Solidity itself constrains the shape.
- Cambrian status: **DONE**. The two literal-`false` fall-throughs (non-deterministic-mode multi-arg `from Entity(...)` and deterministic-mode `from Entity(args)` with `args.len() != identity_count`) are now rejected by validator **V33**. The non-deterministic diagnostic explicitly points at `deterministic_addresses: true` as the multi-arg unblocker. The codegen fall-throughs survive only as `debug_assert!` defence in depth in `gen_from_checks` / `gen_from_checks_det` ([`solidity/evm/route.rs`](../cambrian-transpiler/src/codegen/solidity/evm/route.rs)).
- Tests: `evm6_v33_from_entity_multi_arg_non_det_errors`, `evm6_v33_from_entity_single_arg_non_det_ok`, `evm6_v33_from_entity_wrong_arity_det_errors`, `evm6_v33_from_entity_correct_arity_det_ok`, `evm6_v33_from_entity_extern_skipped` in `cambrian-transpiler/tests/test_validate.rs`.

### 1.27 Hardcoded pragma

- Solidity reference: [pragmas](https://docs.soliditylang.org/en/v0.8.24/layout-of-source-files.html#pragmas).
- Cambrian status: **fixed** — the generator emits `pragma solidity ^0.8.24;` unconditionally; users cannot widen, narrow, or change the SPDX header.
- Codegen site: [`solidity/evm/mod.rs`](../cambrian-transpiler/src/codegen/solidity/evm/mod.rs) (`pragma solidity ^0.8.24;`).
- Priority: P1 — blocks integration with toolchains pinned to a specific minor version, and the SPDX `UNLICENSED` may not be appropriate for production deploys.
- Workaround: post-process the output before feeding to `solc`.
- Scheduled: unscheduled.

### 1.28 `memory` data-location annotation on local reference types — DONE (Phase EVM-15 H2 / Cluster A)

- Solidity reference: [data location](https://docs.soliditylang.org/en/v0.8.24/types.html#data-location) — local variables of `string`, `bytes`, `struct`, and array types must be declared with `memory` (or `storage` / `calldata`).
- Cambrian status: **DONE** — every local variable site (`next_<member>` snapshot temporaries, `_cam_tmp` mapping locals, `let` bindings, route-parameter passthrough) now consistently uses `sol_type_entity(..., in_param=true)`, which threads `memory` onto reference types. Defaults for `Expr::None` / `Expr::EmptyCollection` against a reference-typed slot now expand via `solidity_default_for_member_ty` to `""` (string), `bytes("")` (bytes), `address(0)`, etc., instead of the bare `0` that was previously emitted (and silently miscompiled when the surrounding type was a reference). A new thread-local `LET_BINDING_TYPES` registry lets `infer_let_type_entity` resolve `Expr::Ident` and `Expr::RecordUpdate` against let-bound locals so the right type flows through chained transforms.
- Codegen sites: `sol_type_entity` / `infer_let_type_entity` ([`solidity/core/types.rs`](../cambrian-transpiler/src/codegen/solidity/core/types.rs)); `gen_expr_hoisted` ([`solidity/core/expr.rs`](../cambrian-transpiler/src/codegen/solidity/core/expr.rs)); `gen_mapping_transform_split` ([`solidity/evm/transform.rs`](../cambrian-transpiler/src/codegen/solidity/evm/transform.rs)); route transforms in [`solidity/evm/route.rs`](../cambrian-transpiler/src/codegen/solidity/evm/route.rs).
- Fixtures graduated: `escrow_v2`, `registry`, `voting`, `nft`, `staking`, `dex`, `token`.

### 1.29 Cambrian stdlib helpers (`min` / `max` / `clamp` / `muldiv` / `divc` / `divr` / `divmod`) and HashMap `.contains()` / `.is_empty()` — DONE (Phase EVM-15 H3 / Cluster B)

- Solidity reference: stdlib helpers have no Solidity equivalent; OpenZeppelin's `[Math.sol](https://github.com/OpenZeppelin/openzeppelin-contracts/blob/master/contracts/utils/math/Math.sol)` provides `min` / `max` / `mulDiv`. Mapping membership tests have no built-in form.
- Cambrian status: **DONE**.
  - `min`, `max`, `clamp`, `muldiv`, `divc`, `divr`, `divmod` are emitted as Solidity *free functions* at the top of the generated source — but only when actually called, detected by the AST walkers `stdlib_fn_used_in_expr` / `stdlib_fn_used_in_action` / `stdlib_fn_used_in_program`. The dispatch lives in `gen_stdlib_helpers` + `stdlib_helper_solidity` in [`solidity/core/pure.rs`](../cambrian-transpiler/src/codegen/solidity/core/pure.rs).
  - `m.contains(k)` lowers to `m_exists[k]` (or `m_inner_exists[k1][k2]` for the 2-level case) reusing the EVM-3 sidecar.
  - `m.is_empty()` lowers to `m_keys.length == 0`, reusing the EVM-13 iterable companion (the `_keys` array). `member_is_iterated` was extended so the companion is emitted whenever `is_empty` is reachable.
  - **Stdlib helper / user pure-fn shadowing**: a project that declares `pure fn min(a, b) -> T` shadows the built-in `min` helper at every *bare* call site. `std::math::<name>` always lowers to a dedicated `_cam_std_<name>` helper, so it is never captured by the user function (book-audit slice 1c). `gen_stdlib_helpers` skips any stdlib helper whose name collides with a `program.pure_fns` entry, so the Solidity output contains exactly one definition (Error 1686 "Function with same name and parameter types defined twice" no longer fires). Regression test: `evm_user_pure_fn_shadows_stdlib_helper_no_duplicate_emit`. Surfaced by `examples/uniswap-v2/UniswapV2Pair.cam`.
- Fixtures graduated: `batch1_showcase`, `stdlib_demo`.

### 1.30 Solidity-reserved identifier mangling — DONE (Phase EVM-15 H4 / Cluster C)

- Solidity reference: [reserved keywords](https://docs.soliditylang.org/en/v0.8.24/cheatsheet.html#reserved-keywords).
- Cambrian status: **DONE**.
  - Cambrian identifiers that collide with Solidity keywords or built-in types (`bytes`, `now`, `receive`, `type`, `function`, `address`, `string`, `bool`, …) are mangled with a leading underscore by `sol_sanitize_ident`. The mangling is applied uniformly at every declaration site (parameter lists, `let` bindings, route function names) and every use site (`Expr::Ident`, interface call dispatch in `emit_send`).
  - Tuple destructuring `let (a, b) = expr` now lowers to Solidity's native `(T1 a, T2 b) = expr;` form (previously the codegen tried to assign `a = expr[0]` which Solidity rejects on tuple expressions).
  - **Withdrawn (2026-08-20):** H4 also shipped `rescue` → `try`/`catch`. That analogue is gone (**E26**). `bouncer` is EVM-incompatible.
- Fixtures graduated: `batch1_an`.

### 1.31 Tagged-union enum lowering for payload-bearing variants — DONE (Phase EVM-4 J1-J3)

- Solidity reference: [enums](https://docs.soliditylang.org/en/v0.8.24/types.html#enums) (unit-only); structs are the standard tagged-union surrogate.
- Cambrian status: **DONE**. A Cambrian enum `enum Action { Deposit(u64), Withdraw(u64, String), Reset }` lowers to:
  ```solidity
  enum Action_Tag { Deposit, Withdraw, Reset }
  struct Action {
      Action_Tag tag;
      uint256 deposit_0;     // Deposit(u64)
      uint256 withdraw_0;    // Withdraw(u64, String)
      string  withdraw_1;
      // Reset has no payload positions, so no fields contributed
  }
  ```
  - **Field-naming convention**: `<lowercase_variant>_<positional_index>`. Cross-variant collisions are impossible because the variant prefix segregates them.
  - **Storage trade-off**: every variant always reserves one slot per payload field across **all** variants. `Reset` carries unused `deposit_0` / `withdraw_0` / `withdraw_1` slots even though it never reads them. Users who need slim per-variant storage can manually decompose into multiple top-level members and a unit-tag enum.
  - **Constructor**: `Action::Deposit(amount)` lowers to `Action({tag: Action_Tag.Deposit, deposit_0: amount, withdraw_0: 0, withdraw_1: ""})`. Non-active payload slots use the type-aware `solidity_default_for_member_ty` (e.g. `""` for `string`, `0` for `uint256`).
  - **Match**: `Action::Deposit(amount) => m_balance + amount` rewrites every `Expr::Ident("amount")` in the body to `Expr::FieldAccess(Expr::Ident("action"), "deposit_0")` via `subst_payload_bindings` before lowering. Wildcards (`_`) emit no binding.
  - **Memory annotation**: payload enums are structs, so `sol_type_entity` adds `memory` whenever the type appears as a parameter / return / local — same as records.
- Detection: `is_payload_enum(decl)` returns true iff *any* variant has fields. Unit-only enums keep the simple `enum X { ... }` Solidity form unchanged (regression-tested by `evm4_j1_unit_only_enum_still_emits_solidity_enum`).
- Tests: `tests/test_codegen_evm.rs::evm4_j*_`* plus the `enum_data` fixture in `tests/test_evm_solc.rs::ALL_FIXTURES` (graduation check).
- Fixtures graduated: `enum_data`.

### 1.32 `extern entity` declarations for foreign-contract interfaces — DONE (Phase EVM-6 M1)

- Solidity reference: [interface declarations](https://docs.soliditylang.org/en/v0.8.24/contracts.html#interfaces) — every cross-contract typed call needs an interface in scope at the call site.
- Cambrian status: **DONE**. The previous EVM behaviour emitted a placeholder `interface IExt { /* add route signatures as needed */ }` whenever a typed `~> dest` send referenced an entity not in `program.entities`, which `solc` then rejected at the call site (the placeholder didn't expose the called function). Phase EVM-6 M1 introduces first-class syntax:
  ```cambrian
  extern entity Token {
      route transfer(amount: U256);
      view route balanceOf(who: address) -> U256;
      accept route deposit() -> bool;   // payable
  }
  ```
  - **Grammar**: new top-level `extern entity Name { ... }` block whose body is a sequence of route signatures (no bodies). Modifiers `view route` and `accept route` map to Solidity `view` and `payable` respectively.
  - **AST**: `Program.extern_entities: Vec<ExternEntity>` (alongside `entities`), with `ExternRoute { name, params, return_type, is_view, is_payable, span }`.
  - **EVM codegen**: `gen_extern_interface_decl` emits a populated `interface IName { ... }` per declaration. The previous empty-stub fallback survives only as a `debug_assert!` defence in depth — validator E22 should make it unreachable. `emit_var_call` reads return types from `ExternRoute` for `var x = msg(args) ~> ext_dest;` captures.
  - **Acki Nacki target**: extern entity declarations are inert (no code is emitted) — covered by `evm6_extern_entity_acki_nacki_no_op`.
  - **Validation hooks**:
    - **V30** — duplicate `extern entity Name` within the program, or collision with a real `entity Name` declaration.
    - **V31** — duplicate route signature inside an `extern entity` block.
    - **V23 fallback** — typed sends to extern routes use the extern declaration's signature for arity / return-type checks.
    - **V32** — `deploy Entity(args)` is *skipped* when the deploy target is an extern entity (signature unknown).
    - **V33** — `from Entity(args)` is *skipped* when the from-clause target is an extern entity.
    - **E22** (EVM-target) — typed `~> dest` where `dest: Address<X>` and `X` is neither in `program.entities` nor `program.extern_entities` is a hard error pointing at the extern declaration as the fix.
- Tests: `evm6_m1_extern_entity_emits_populated_interface`, `evm6_m1_extern_entity_payable_route_emits_payable_modifier`, `evm6_m1_unknown_external_entity_still_emits_unreachable_stub_in_release`, updated `evm_gt8_cross_entity_interface_emission_pins_both_modes` (single-file flavour now declares `extern entity Token { ... }`), `evm6_m1_extern_entity_compiles_with_solc`, `evm6_v30_`*, `evm6_v31_*`, `evm6_v23_extern_route_lookup_succeeds`, `evm6_e22_*`, `evm6_extern_entity_acki_nacki_no_op`. The fixture `contracts/extern_token_caller.cam` is graduated into `ALL_FIXTURES`.

### 1.33 Temporal refs and effect-argument snapshots

- Cambrian status: **DONE** (book-audit slice 1b). Same-phase `^m` in a phased route reads the phase's `next_m` local. Unphased routes and unphased init bodies snapshot committed member reads used as effect arguments (`_pre_*`), exactly like named phases. `^m_map[k]` flushes `m_map`'s pending writes ahead of the reading transform.
- Remaining gap: a transform that reads **both** `^m_map[...]` and bare `m_map[...]` sees the post-write value for the bare read too, and a send argument reading `m_map` in the same route is snapshotted after that flush. Lean models both correctly.
- Tests: `tests/test_book_audit_fixes.rs`.

### 1.34 Type aliases and field-typed `let` bindings

- Cambrian status: **DONE** (book-audit slice 1d). Before EVM lowering, `solidity/core/alias.rs` rewrites every declared type (members, params, returns, records, enums, events, errors, constants, macros, externs, libraries, test/fuzz/invariant params) to its alias-free form; entity- and library-local aliases shadow program-scope ones. Previously any alias name fell through the type mapper to `uint256` (`type Owner = address` produced `mapping(uint256 => …)`). The Foundry, revm and cargo-fuzz test generators apply the same pass.
- `let a = m_vec[i].owner` (a field of any record-typed expression, not only of an identifier) takes the field's Solidity type instead of `uint256`.
- Tests: `tests/test_book_audit_fixes.rs`.

### 1.35 Sign-changing casts and `evm::keccak256`

- Cambrian status: **DONE** (book-audit stage 2). `x as uN` from a signed source lowers to `_toUintN(_camI2U(int256(x)))` and `x as iN` from an unsigned source to `_toIntN(_camU2I(uint256(x)))`. Solidity rejects a sign change combined with a width change, and a bare `uint256(int256)` wraps; the helpers panic on a negative value / a value above `type(int256).max`, matching the narrowing rule in `LANGUAGE.md`. Mixed-sign operands with no covering signed type (`u128` / `U256` with a signed operand) are rejected by **V69**.
- `evm::keccak256(x)` (one argument) lowers to `uint256(keccak256(abi.encode(x)))`; unsupported `evm::<name>` / arities are **E16** at `--check` time instead of an `Unsupported` stub at codegen.

### 1.36 Constructs that failed `forge build`

- Cambrian status: **DONE** (book-audit stage 3). Each of these transpiled but produced Solidity that solc rejected:
  - Wrapping ops (`+%` / `-%` / `*%`) in a `pure fn` or library fn: `_wadd`-family helpers are emitted at file scope when pure code uses them (contract-internal otherwise). Signed operands use `_wadds` / `_wsubs` / `_wmuls` over `int256`; the narrowing cast back to the operand type truncates, which gives wrap-around.
  - `v.is_empty()` / `m.len()` / `m.is_empty()` on `HashMap` members read the keys companion (`m_keys.length`), which is now emitted for these methods and for `.exists` inside entity macros.
  - A `let` name bound twice in one route or `pure fn` scope: route bodies hoist the name once; `pure fn` bodies rename the inner binding (`x_1`, …).
  - `some(<literal>)` into `Option<T>` takes the struct type from the expected `Option<T>`; `= none` member defaults and `bytes` members get typed defaults (`""`).
  - `let xs = array(...)` and `let s = std::str::format(...)` bind with their real Solidity types; `std::crypto::sha256(...)` (`bytes`) assigned to a `Vec<u8>` member is copied through `_cam_bytes_to_u8s`.
  - `Lib.f(a, b).g(c)` with `using Lib for T`: the qualified call's return type types the chained receiver.
  - Full 40-digit address literals in invariant `senders { }` / `exclude senders { }` / `ctx { msg::sender }` lower to `address(uint160(...))`.
  - Program-scope records whose fields are other records / enums keep the struct / enum field type (previously erased to `uint256`).
  - A value `match` with exhaustive patterns emits the last arm as the unconditional `else` instead of a numeric `0` fallback, so `bool` / enum arm bodies type-check.
- Tests: `tests/test_book_audit_fixes.rs` (`stage3_evm_constructs_compile_and_run`, `invariant_senders_accept_full_width_addresses`).

---

## 2. Solidity features missing from the Cambrian surface

Where § 1 enumerates codegen fallbacks, § 2 enumerates features that
have **no Cambrian surface syntax at all**. Adding any of them
requires grammar / AST / validator changes, not just a new branch in
`solidity/`.

### 2.1 Type system

#### 2.1.1 `address payable` distinction

- Solidity reference: [address types](https://docs.soliditylang.org/en/v0.8.24/types.html#address).
- Cambrian status: **partial** — Cambrian has only `address`; the codegen wraps low-level sends in `payable(...)` casts at the call site ([`solidity/evm/emitter.rs`](../cambrian-transpiler/src/codegen/solidity/evm/emitter.rs)) but the type system does not track payability.
- Priority: P1
- Workaround: the call-site cast is enough for `~> dest with { value: ... }`; arbitrary `addr.transfer(v)` / `addr.send(v)` patterns are not expressible.
- Scheduled: unscheduled.

#### 2.1.2 Fixed-size arrays `T[N]`

- Solidity reference: [arrays](https://docs.soliditylang.org/en/v0.8.24/types.html#arrays).
- Cambrian status: **no** — only dynamic `Vec<T>` (`T[]`).
- Priority: P1 — fixed arrays matter for storage-layout-sensitive code (pre-image/Merkle proofs, fixed-length tuples).
- Workaround: dynamic `Vec<T>` with a length-checking `where` clause.
- Scheduled: unscheduled.

#### 2.1.3 `bytesN` (`bytes1`..`bytes32`)

- Solidity reference: [fixed-size byte arrays](https://docs.soliditylang.org/en/v0.8.24/types.html#fixed-size-byte-arrays).
- Cambrian status: **partial** — `bytes4` and `bytes32` are first-class scalars (route parameters, members, pure-function returns, EVM + Lean lowering), and `pubkey` also lowers to `bytes32`. The remaining widths (`bytes1`..`bytes3`, `bytes5`..`bytes31`) are absent.
- Priority: P2 — the two widths that carry real traffic (`bytes4` selectors, `bytes32` words) are covered; the rest are rare.
- Workaround: model as `U256` and bit-shift.
- Scheduled: `bytes4` shipped alongside ERC-165 support (§ 2.13.1) and `bytes32` alongside EIP-2612 permit in `stdlib/token`; the other widths unscheduled.
- Note on carriers: in Lean, `bytes32` shares the `U256` carrier and `bytes4` does not. Widening a full word loses nothing; widening a four-byte selector would make two different selectors equal after truncation, so `bytes4` maps to `BitVec 32` instead.

#### 2.1.4 Custom Solidity `error`

- Solidity reference: [errors](https://docs.soliditylang.org/en/v0.8.24/contracts.html#errors-and-the-revert-statement).
- Cambrian status: **done** — `error Name(T1, T2, ...);` declarations at program or entity scope; `throw Name(args)` action and `: throw Name(args)` in `where` / `from` clauses; lowers to first-class Solidity custom errors (`error` decl + `revert Name(args);`). On Acki Nacki the same syntax falls back to a deterministic FNV-1a 16-bit numeric code so existing TVM workflows are unaffected. Validators V38 (undeclared error) and V39 (arity / type mismatch) gate misuse.
- Priority: ~~P0~~ DONE.
- Workaround: stick to numeric `throw N` if you need to interop with consumers that haven't yet adopted the custom-error ABI.
- Scheduled: shipped in PLAN_EVM-P0-D.

#### 2.1.5 User-defined value types (`type X is uint256`)

- Solidity reference: [user-defined value types](https://docs.soliditylang.org/en/v0.8.24/types.html#user-defined-value-types).
- Cambrian status: **no** — Cambrian `type X = U256` is a transparent alias; it does not produce a UDVT in Solidity. Unknown `Type::Simple` names silently become `uint256` (1.22).
- Priority: P2
- Workaround: rely on Cambrian-side type aliases plus manual casts.
- Scheduled: unscheduled.

#### 2.1.6 `unchecked { }` arithmetic blocks

- Solidity reference: [checked or unchecked arithmetic](https://docs.soliditylang.org/en/v0.8.24/control-structures.html#checked-or-unchecked-arithmetic).
- Cambrian status: **partial** — Cambrian has per-operator wrapping ops `+%`, `-%`, `*%`, lowered through `_wadd`/`_wsub`/`_wmul` helpers ([`solidity/core/pure.rs`](../cambrian-transpiler/src/codegen/solidity/core/pure.rs)); no block-level `unchecked { ... }` for cheap loop counters.
- Priority: P2 (reclassified from P0). The motivating idiom — Solidity's `for (uint256 i = 0; i < n;) { ...; unchecked { ++i; } }` — does not transfer to Cambrian, which has no C-style stepped `for` loop and instead exposes ranges / iterator chains. Wrapping arithmetic semantics are already reachable via `+%` / `-%` / `*%`, so the only remaining benefit of a block form is saving the per-call wrapper helper, which is a micro-optimisation rather than a feature gap.
- Workaround: use `+%` (and friends) for the operations that need to wrap; rely on standard checked arithmetic everywhere else.
- Scheduled: unscheduled.

#### 2.1.7 Packed structs

- Solidity reference: [storage packing](https://docs.soliditylang.org/en/v0.8.24/internals/layout_in_storage.html#layout-of-state-variables-in-storage).
- Cambrian status: **done** — narrow numeric types (`u8`/`u16`/`u32`/`u64`/`u128`, `i8`/`i16`/`i32`/`i64`/`i128`) survive into Solidity at their declared widths so structs and storage members pack the same way as hand-written Solidity. Implicitly-`uint256` results from arithmetic, `block.timestamp`, etc. are narrowed back to the declared width via `uintN(...)` / `intN(...)` casts only when needed (no redundant casts when the source type already matches). The `actual_sol_type` / `maybe_narrow_cast` helpers in `evm_types.rs` model Solidity's widening rules to keep this lossless.
- Priority: ~~P0~~ DONE.
- Workaround: none needed.
- Scheduled: shipped in PLAN_EVM-P0-B.

### 2.2 Function modifiers / visibility

#### 2.2.1 User-defined `modifier`

- Solidity reference: [function modifiers](https://docs.soliditylang.org/en/v0.8.24/contracts.html#function-modifiers).
- Cambrian status: **no** — `where` and `from` clauses cover the common cases (Ownable-style, ReentrancyGuard); arbitrary `modifier` declarations do not exist.
- Priority: P1
- Workaround: copy the same `where` / `from` chain on every route.
- Scheduled: unscheduled.

#### 2.2.2 `payable` on routes

- Solidity reference: [function visibility specifiers](https://docs.soliditylang.org/en/v0.8.24/contracts.html#visibility-and-getters), [payable functions](https://docs.soliditylang.org/en/v0.8.24/types.html#address).
- Cambrian status: **DONE** (Phase EVM-15 H5).
  - `accept route(...)` continues to lower to `external payable`.
  - Any route that reads `msg::value` anywhere in its reachable AST (route body, `where` clauses, or any member transform fired by the route) automatically gains the `payable` modifier on the emitted Solidity function — no Cambrian syntax change required. Detection: `route_uses_msg_value` in `cambrian-transpiler/src/codegen/solidity/`.
  - `init` constructors are now always emitted `payable` so that `deploy Entity with { value: ..., ... }(args)` deploy sites work end-to-end (deploys without value remain harmless on a `payable` constructor).
  - Pinned by `evm15_h5_route_reading_msg_value_is_payable` and `evm15_h5_msg_value_in_transform_only_marks_route_payable` in `cambrian-transpiler/tests/test_codegen_evm.rs`.
- Priority: closed.

#### 2.2.3 `virtual` / `override`

- Solidity reference: [function overriding](https://docs.soliditylang.org/en/v0.8.24/contracts.html#function-overriding).
- Cambrian status: **no** — Cambrian has no inheritance.
- Priority: P1 — paired with 2.3.
- Scheduled: unscheduled.

#### 2.2.4 True Solidity `private`

- Solidity reference: [visibility](https://docs.soliditylang.org/en/v0.8.24/contracts.html#visibility-and-getters).
- Cambrian status: **partial** — Cambrian `private route` lowers to a Solidity `internal` function `_name`, and `call name(...)` lowers to a direct internal call (no `this.`), so `msg.sender` inside the callee stays the original caller, as on Lean. A non-private route that is a `call` target is emitted `public` so it can be called internally. See `route_function_visibility` / `EvmActionEmitter::emit_call_route`. True `private` (subclass-invisible) is not reachable.
- Priority: P2 — `private` and `internal` only diverge in inheritance, which Cambrian doesn't have.
- Scheduled: unscheduled.

### 2.3 Inheritance & code reuse

#### 2.3.1 Contract inheritance

- Solidity reference: [inheritance](https://docs.soliditylang.org/en/v0.8.24/contracts.html#inheritance).
- Cambrian status: **no** — single contract per `entity`. No `is`-clause, no abstract contracts.
- Priority: P1 — blocks direct use of OpenZeppelin and EIP-2535-style facets.
- Workaround: copy-paste members and routes across entities.
- Scheduled: unscheduled.

#### 2.3.2 `library` and `using for`

- Solidity reference: [libraries](https://docs.soliditylang.org/en/v0.8.24/contracts.html#libraries), [using for](https://docs.soliditylang.org/en/v0.8.24/contracts.html#using-for).
- Cambrian status: **done** — [PLAN_LIBRARY.md](../PLAN_LIBRARY.md) Phases 2-3 ship the full surface.

  - **Phase 2 — `using` method-call sugar.** Lifts free `pure fn` helpers to
    receiver-position calls:

    ```cambrian
    pure fn add_capped(a: u256, b: u256, cap: u256) -> u256 { ... }
    pure fn sub_capped(a: u256, b: u256) -> u256 { ... }

    using { add_capped, sub_capped } for u256;
    // or, attach a whole library at once (Phase 3):
    using MathHelpers for u256;
    ```

    `x.add_capped(y, cap)` rewrites to `add_capped(x, y, cap)` at AST-resolve
    time (pre-codegen pass, `src/using_rewrite.rs`). No runtime cost; the
    EVM and Acki Nacki backends both emit the same calls they would for the
    desugared form. Method-call collisions with `Vec` / `HashMap` / `Address`
    / `String` built-ins are gated by **V55**; undeclared
    `pure fn` / library references by **V56**; receiver-type mismatches by
    **V57**.

  - **Phase 3 — `library` keyword.** Declares a named scope of `pure fn` +
    `const` + `type` items that lower to a Solidity `library` block on EVM
    and a flat set of top-level Rust items on Acki Nacki. Library bodies
    may not access state or temporals (enforced by **V48**, reusing the V4
    purity walker); the parser rejects any non-`pure fn` / `const` / `type`
    body item (**V47**).

    ```cambrian
    library SafeMath {
        pure fn add(a: u256, b: u256) -> u256
            where (a + b >= a) : throw 0x11 { a + b }
        pure fn sub(a: u256, b: u256) -> u256
            where (a >= b) : throw 0x11 { a - b }
    }

    using SafeMath for u256;
    ```

    On EVM, the library lowers to:

    ```solidity
    library SafeMath {
        function add(uint256 a, uint256 b) internal pure returns (uint256) { ... }
        function sub(uint256 a, uint256 b) internal pure returns (uint256) { ... }
    }
    ```

    Library-scoped `pure fn`s call out as `SafeMath.add(a, b)` (qualified by
    library name); free `pure fn`s continue to lower to top-level free
    functions (no behavioural change for existing fixtures).

    **Lean:** library `pure fn`s emit to `Cambrian.Generated.Pure` as
    `LibName_fnName`; `Lib::fn(args)` lowers to qualified calls (audit
    `test_audit_sd01_library_pure_fn_lean`). Chained `HashMap.values().fold`
    / `.keys().fold` on map parameters in `pure fn` bodies: SD-01-VFOLD closed
    on Lean; `cambrian-predict` mirror tracked as **PREDICT-SD01-VFOLD**
    (no predictable corpus yet).

    **Constructor `payable`:** default `payable` ctor + factory forwards
    `msg.value` (EVM-15 H4). Opt out via `evm.allow_constructor_payable:
    false` in `project.yaml` (CH WP-C; audit `test_audit_ch_factory_scaffold`).

  - **Phase 1 — multi-file `import "./shared.cam"`** lets libraries live
    in a separate `.cam` file shared across projects; see § 2.11.1.

- Workaround (pre-Phase-3): top-level `pure fn`s + `using { ... } for T;`
  cover most cases. The `library Name { ... }` block additionally provides
  bytecode-size savings (one shared `JUMP` target instead of inlining) and
  symbol scoping.
- Priority: ~~P1~~ DONE.
- Scheduled: PLAN_LIBRARY.md Phases 1-3.

#### 2.3.3 Multiple inheritance / C3 linearization

- Solidity reference: [multiple inheritance](https://docs.soliditylang.org/en/v0.8.24/contracts.html#multiple-inheritance-and-linearization).
- Cambrian status: **no**.
- Priority: P2.
- Scheduled: unscheduled.

#### 2.3.4 Constructor chaining (`Base(...)` initializer lists)

- Solidity reference: [arguments for base constructors](https://docs.soliditylang.org/en/v0.8.24/contracts.html#arguments-for-base-constructors).
- Cambrian status: **no** — only one synthesized constructor per entity.
- Priority: P1 (depends on 2.3.1).
- Scheduled: unscheduled.

### 2.4 Events

#### 2.4.1 `event` declarations and `emit`

- Solidity reference: [events](https://docs.soliditylang.org/en/v0.8.24/contracts.html#events).
- Cambrian status: **done** — `event Name(T1 a, indexed T2 b, ...);` declarations at program or entity scope; `emit Name(args);` action lowers to a Solidity `emit Name(args);` with narrow-int casts applied per declared parameter type. On non-EVM targets (Acki Nacki) `emit` is a no-op (with a comment) so contracts remain portable. Validators V34 (undeclared event), V35 (arity / type mismatch), V36 (≤ 3 indexed) gate misuse.
- Priority: ~~P0~~ DONE.
- Workaround: none needed.
- Scheduled: shipped in PLAN_EVM-P0-C.

#### 2.4.2 Indexed event parameters

- Solidity reference: [indexed parameters](https://docs.soliditylang.org/en/v0.8.24/contracts.html#events).
- Cambrian status: **done** — `indexed` modifier is part of the event-param grammar. Topic count (≤ 3 for non-anonymous events) is enforced by V36.
- Priority: ~~P0~~ DONE.
- Scheduled: shipped in PLAN_EVM-P0-C.

#### 2.4.3 Anonymous events

- Solidity reference: [anonymous events](https://docs.soliditylang.org/en/v0.8.24/contracts.html#events).
- Cambrian status: **no**.
- Priority: P2.

### 2.5 Errors

#### 2.5.1 `revert("string")` with author-supplied message

- Solidity reference: [revert statement](https://docs.soliditylang.org/en/v0.8.24/control-structures.html#revert-statement-and-expression).
- Cambrian status: **partial** — only numeric `throw N` (lowers to the canonical `"throw(N)"` revert string the test harness matches on, [`solidity/evm/route.rs`](../cambrian-transpiler/src/codegen/solidity/evm/route.rs)). No author-supplied strings.
- Priority: P1.
- Workaround: keep an off-chain error-code → message mapping.
- Scheduled: unscheduled.

#### 2.5.2 Custom errors (`error Foo(uint256); revert Foo(x);`)

- See § 2.1.4 (same surface gap, listed in the type-system section).
- Status: **done** — shipped in PLAN_EVM-P0-D.

#### 2.5.3 `assert(...)`

- Solidity reference: [error handling](https://docs.soliditylang.org/en/v0.8.24/control-structures.html#error-handling-assert-require-revert-and-exceptions).
- Cambrian status: **no**.
- Priority: P2 — `assert` is mostly used as an invariant marker; covered structurally by `where` and `check` (in `invariant` blocks).
- Scheduled: unscheduled.

### 2.6 Low-level / EVM intrinsics

#### 2.6.1 `assembly { }` / Yul

- Solidity reference: [inline assembly](https://docs.soliditylang.org/en/v0.8.24/assembly.html).
- Cambrian status: **no** — there is no embedded-Solidity escape hatch at all.
- Priority: P1 — indispensable for hand-tuned crypto, gas-critical math, and EIP-1153 transient storage.
- Workaround: pre-compute results off-chain and pass them in; pull values from a separate hand-written contract via `~>`.
- Scheduled: unscheduled.

#### 2.6.2 `staticcall` / `delegatecall`

- Solidity reference: [members of address](https://docs.soliditylang.org/en/v0.8.24/units-and-global-variables.html#members-of-address-types).
- Cambrian status: **no** — only high-level interface calls and low-level `.call{value:}` are emitted ([`solidity/evm/emitter.rs`](../cambrian-transpiler/src/codegen/solidity/evm/emitter.rs)).
- Priority: P1 — `delegatecall` is required for proxy patterns and EIP-2535.
- Scheduled: unscheduled.

#### 2.6.3 `addr.balance` for arbitrary addresses

- Solidity reference: same as 2.6.2.
- Cambrian status: **yes** — `evm::balance(addr)` lowers to `addr.balance`. `sys::balance` continues to read the own-contract balance.
- Priority: ~~P1~~ resolved.
- Scheduled: **done in batch 1** (see Phase EVM-14).

#### 2.6.4 `addr.transfer` / `addr.send`

- Solidity reference: [members of address types](https://docs.soliditylang.org/en/v0.8.24/units-and-global-variables.html#members-of-address-types).
- Cambrian status: **no** (only `~> addr with { value: v }` which lowers to `.call{value: v}("")`).
- Priority: P2 — `transfer`/`send` are no longer recommended (2300-gas stipend issues post-Istanbul).
- Scheduled: unscheduled.

#### 2.6.5 `selfdestruct(payable)`

- Solidity reference: `[selfdestruct](https://docs.soliditylang.org/en/v0.8.24/units-and-global-variables.html#contract-related)`. EIP-6780 redefines its semantics in Cancun.
- Cambrian status: **no** — only mentioned as a future `evm::selfdestruct` ([`solidity/core/expr.rs`](../cambrian-transpiler/src/codegen/solidity/core/expr.rs)).
- Priority: P2.
- Scheduled: unscheduled.

#### 2.6.6 `block.`* surface

- Cambrian status: **yes** — `sys::timestamp`, `sys::block_number`, `sys::prevrandao`, `sys::coinbase`, `sys::chainid`, `sys::basefee`, `sys::balance`, `sys::address`, `sys::gas_left`, and `sys::blobbasefee` (EIP-4844 / Cancun) are wired ([`solidity/core/expr.rs`](../cambrian-transpiler/src/codegen/solidity/core/expr.rs)).
- Priority: ~~P1~~ resolved.
- Scheduled: **done in batch 1** (see Phase EVM-14).

#### 2.6.7 `blockhash(blockNumber)`

- Solidity reference: [block and transaction properties](https://docs.soliditylang.org/en/v0.8.24/units-and-global-variables.html#block-and-transaction-properties).
- Cambrian status: **yes** — `evm::blockhash(n)` lowers to `uint256(blockhash(n))`. Returns `U256` so it composes with Cambrian arithmetic.
- Priority: ~~P1~~ resolved.
- Scheduled: **done in batch 1** (see Phase EVM-14).

#### 2.6.8 `tx.origin` / `tx.gasprice`

- Cambrian status: **yes** — `sys::origin` lowers to `tx.origin` (typed as `address`); `sys::gasprice` lowers to `tx.gasprice`.
- Priority: ~~P2~~ resolved.
- Scheduled: **done in batch 1** (see Phase EVM-14).

#### 2.6.9 `msg.data` / `msg.sig`

- Cambrian status: **no** — `msg::body` is explicitly TVM-only on EVM ([`solidity/core/expr.rs`](../cambrian-transpiler/src/codegen/solidity/core/expr.rs) inner `_ => None`).
- Priority: P1 — `msg.sig` is the cornerstone of selector-based dispatch / fallback routing.
- Scheduled: unscheduled.

#### 2.6.10 `gasprice`

- Cambrian status: **yes** — covered by `sys::gasprice` (see 2.6.8).
- Priority: ~~P2~~ resolved.
- Scheduled: **done in batch 1** (see Phase EVM-14).

#### 2.6.11 Transient storage (EIP-1153 `TSTORE` / `TLOAD`)

- Solidity reference: `transient` storage location, available since Solidity 0.8.24 with Cancun.
- Cambrian status: **no**.
- Priority: P1 — increasingly important for reentrancy guards and OZ v5 patterns.
- Scheduled: unscheduled.

### 2.7 Cryptography

#### 2.7.1 `sha256` / `ripemd160` (precompiles)

- Solidity reference: [mathematical and cryptographic functions](https://docs.soliditylang.org/en/v0.8.24/units-and-global-variables.html#mathematical-and-cryptographic-functions).
- Cambrian status: **yes** — `evm::sha256(args...)` lowers to `uint256(sha256(abi.encodePacked(args...)))` and `evm::ripemd160(args...)` lowers to `uint256(uint160(ripemd160(abi.encodePacked(args...))))`. Both are tracked as pure intrinsics by the validator (V4-friendly inside `pure fn`).
- Priority: ~~P1~~ resolved.
- Scheduled: **done in batch 1** (see Phase EVM-14).

#### 2.7.2 EIP-712 typed data hashing helpers

- Solidity reference: [EIP-712](https://eips.ethereum.org/EIPS/eip-712).
- Cambrian status: **partial** — buildable manually using `hashOf(...)` (`abi.encode` flavour) plus `evm::keccak256Packed(...)` (`abi.encodePacked` flavour). See `examples/uniswap-v2/ERC20.cam`.
- Priority: P1 — EIP-712 is mandatory for permit / EIP-2612 / OZ Governor / safe-transactions.
- Workaround: hand-rolled type hashes and `\x19\x01` prefix. Verbose but correct.
- Scheduled: unscheduled.

#### 2.7.3 EIP-191 signed messages

- Solidity reference: [EIP-191](https://eips.ethereum.org/EIPS/eip-191).
- Cambrian status: **partial** — same building blocks as 2.7.2.
- Priority: P1.
- Scheduled: unscheduled.

### 2.8 Control flow

#### 2.8.1 `while` / `do-while`

- Solidity reference: [loop statements](https://docs.soliditylang.org/en/v0.8.24/control-structures.html#for-loop).
- Cambrian status: **no** — no AST node (`[ast.rs](../cambrian-transpiler/src/ast.rs)` has only `Expr::For`).
- Priority: P1 — many algorithms need bounded `while`s (binary search, Newton-Raphson termination on convergence).
- Workaround: bounded `for i in 0..MAX` with an `if` early-exit pattern (but no `break`).
- Scheduled: unscheduled.

#### 2.8.2 `break` / `continue`

- Cambrian status: **no**.
- Priority: P1 (with 2.8.1).
- Scheduled: unscheduled.

#### 2.8.3 General `try` / `catch` with typed errors

- Solidity reference: [try/catch](https://docs.soliditylang.org/en/v0.8.24/control-structures.html#try-catch).
- Cambrian status: **no**. `rescue`/`recover` is Acki Nacki bounce only (**E26** on EVM). There is **no contained-failure / try-catch mechanism on EVM yet**. Route abort remains `where` / `throw`.
- Priority: P1.
- Workaround: none on EVM; use Acki Nacki for bounce recovery.
- Scheduled: unscheduled (former PLAN_EVM-8 `try`/`catch` analogue withdrawn 2026-08-20).

#### 2.8.4 Function pointers / `function` types

- Solidity reference: [function types](https://docs.soliditylang.org/en/v0.8.24/types.html#function-types).
- Cambrian status: **no**.
- Priority: P2.
- Scheduled: unscheduled.

#### 2.8.5 Named returns

- Solidity reference: [returning multiple values](https://docs.soliditylang.org/en/v0.8.24/control-structures.html#destructuring-assignments-and-returning-multiple-values).
- Cambrian status: **no** — generated `returns (...)` declarations are positional.
- Priority: P2 — cosmetic.
- Scheduled: unscheduled.

#### 2.8.6 Mixed-type tuple destructure in `let` (already in 1.12)

- See § 1.12. Listed twice because it surfaces both as a codegen fallback and as a language-surface limitation.

#### 2.8.7 HashMap iteration (`m.keys()` / `m.iter()` / `m.values()` / `for (k, v) in m`)

- Solidity reference: n/a — Solidity `mapping(K => V)` exposes neither `length` nor a key enumeration. The standard idiom is the OpenZeppelin `EnumerableMap` library, which keeps a parallel `K[]` index alongside the mapping and does swap-pop on remove.
- Cambrian status: **DONE** (Batches 1, D, and F). When any expression in the entity calls `.keys()` / `.values()` / `.iter()` / `.fold` / `.collect()` / `.filter` / `.map` / `.take` / `.enumerate` on a HashMap member, the EVM backend auto-emits a parallel `K[] m_keys` index array, a private `mapping(K => uint256) m_keys_index` reverse-lookup, and forces the existence-flag mapping `mapping(K => bool) m_exists` to be present. `gen_mapping_transform_split` then injects guarded `m_keys.push` / swap-pop maintenance into every `insert` / `update` / `remove` against the iterated member.
  - `for k in m.keys() { body }` walks the parallel array via the existing `gen_for_loop` machinery (Batch 1).
  - `for (k, v) in m.iter() { body }` and the bare-`m` sugar `for (k, v) in m { body }` both walk `<m>_keys` once per iteration and bind `K _k = <m>_keys[_i]; V _v = <m>[_k];` (Batch D, leveraging Batch A's tuple binder).
  - `for v in m.values() { body }` walks `<m>_keys` and binds the V scalar via `ChainStage::Values` projection.
  - `m.values().collect()` materialises a `V[] memory` of length `<m>_keys.length`.
  - `m.iter().fold(...)` / `m.values().fold(...)` (and any chain prefix from Batch B) fuses into a single Solidity `for` loop terminated by the fold accumulator update.
  - Non-iterated HashMaps are unaffected: detection is purely on the use site, so contracts that never iterate keep the prior gas-cheap mapping-only layout.
- **Ordering guarantees (Batch F).** The iteration order over `<m>_keys` follows mechanically from the maintenance code:
  1. **Insert preserves order** — first-time `insert(k, v)` (and the bare `register(...)` shape) appends `k` at the tail of `<m>_keys`. Subsequent `insert` of an already-present key is a no-op against the parallel array.
  2. **Remove uses swap-pop** — `remove(k)` overwrites the removed slot with the last key, rewrites the relocated key's reverse-index entry, and pops the tail. The relocated key inherits the removed slot's position; all other entries keep their slots.
  3. **Re-inserting a removed key appends** — because `<m>_exists[k]` is cleared on remove, the next `insert(k, v')` falls through the existence guard and appends at the new tail.
  Pinned by `evm13_insert_preserves_order_via_tail_push`, `evm13_remove_uses_swap_pop_and_clears_existence_flag`, and `evm13_reinsert_after_remove_takes_append_path` in `[cambrian-transpiler/tests/test_evm_solc.rs](../cambrian-transpiler/tests/test_evm_solc.rs)`. The companion `evm_keys_fixture_compiles_with_solc` test compiles the same fixture through real solc, so the ordering proof is grounded in code that solc accepts.
- Codegen sites: detection helper `member_is_iterated` ([`solidity/evm/analysis.rs`](../cambrian-transpiler/src/codegen/solidity/evm/analysis.rs)); storage emission alongside the existing `_exists` companion in `gen_entity_contract`; insert/update/remove maintenance in `gen_mapping_transform_split`; iter-source resolution in `resolve_iter_source` (handles `.keys()` / `.iter()` / `.values()` / bare `Ident`); chain-fusion lowering in `lower_iter_chain` (with `ChainStage::Values` projection); type inference in `infer_let_type_entity` and `infer_iter_elem_type_entity`. End-to-end fixture: `[contracts/keys_evm.cam](../contracts/keys_evm.cam)`. Unit tests: `evm13_`* in `[cambrian-transpiler/tests/test_codegen_evm.rs](../cambrian-transpiler/tests/test_codegen_evm.rs)`. Solc-integration tests: `evm_keys_fixture_`* and the ordering-guarantee suite in `[cambrian-transpiler/tests/test_evm_solc.rs](../cambrian-transpiler/tests/test_evm_solc.rs)`.
- Priority: P1 — closes out the HashMap-iteration story (registry walks, "list all owners", "sum all balances", `.iter().filter(...).fold(...)` aggregations).
- Cost when used: ~22k gas per first-time insert (one extra SSTORE for the existence flag + one for the index push), ~5k extra on remove (swap-pop reads + length update). Reads from the underlying mapping itself are unchanged.
- Scheduled: `PLAN_EVM-13` — DONE.

### 2.9 ABI

#### 2.9.1 General `abi.encode` / `abi.decode` / `abi.encodeWithSelector`

- Solidity reference: [ABI encoding and decoding functions](https://docs.soliditylang.org/en/v0.8.24/units-and-global-variables.html#abi-encoding-and-decoding-functions).
- Cambrian status: **partial** — `hashOf(...)` already wraps in `abi.encode`; `evm::keccak256Packed` wraps in `abi.encodePacked`; named-send to an untyped address synthesises an `abi.encodeWithSignature` payload ([`solidity/core/expr.rs`](../cambrian-transpiler/src/codegen/solidity/core/expr.rs)). No general builtin for arbitrary tuples or for `abi.decode`.
- Priority: P1 — needed for ERC-1155 batch ops, multi-call, generic adapters.
- Scheduled: unscheduled.

#### 2.9.2 First-class `bytes4` selectors

- Cambrian status: **partial** — `bytes4` is a real type now, so a selector can be a parameter, a member, a literal and a comparison operand (this is what makes `supportsInterface(bytes4)` expressible). What is still missing is a way to *compute* a selector from a signature — no `Entity.route.selector` and no `bytes4(keccak256("f(uint256)"))`, so an interface id has to be written as a literal with the derivation shown in a comment.
- Priority: P2 — the literal is auditable and stable; computing it in-contract saves a comment, not a class of bug.
- Scheduled: type shipped with § 2.13.1; selector arithmetic unscheduled.

### 2.10 Constructor / lifecycle

#### 2.10.1 `payable` constructor

- Solidity reference: [creating contracts](https://docs.soliditylang.org/en/v0.8.24/control-structures.html#creating-contracts-via-new).
- Cambrian status: **DONE** (Phase EVM-15 H4 + H5). Generated constructors are emitted `payable` unconditionally, so `deploy Entity with { value: ..., ... }(args)` works end-to-end. The `init` body is free to read `msg::value` like any other route. Marking a constructor `payable` is benign for value-less deploys, and the `wrap-on-deploy` (WETH-style) idiom is now expressible.
- Priority: closed.

#### 2.10.2 `fallback()` / `receive()`

- Solidity reference: [special functions](https://docs.soliditylang.org/en/v0.8.24/contracts.html#fallback-function), [receive ether function](https://docs.soliditylang.org/en/v0.8.24/contracts.html#receive-ether-function).
- Cambrian status: **done** — declaring a route named `receive` or `fallback` (no params, no return, no `view` / `pure`) lowers to Solidity's special functions. `receive` is always emitted `payable`; `fallback` is auto-`payable` if the body reads `msg::value`. Validators V40 (route shape; `receive` must be `accept`) and V41 (no duplicates) gate misuse, and these rules only fire for the EVM target so TVM contracts that happen to use `receive` as a method name still compile.
- Priority: ~~P0~~ DONE.
- Workaround: none needed.
- Scheduled: shipped in PLAN_EVM-P0-E.

#### 2.10.3 ETH-receiving entities

- Cambrian status: **done** — `accept route(...)` flag covers named routes, and `receive() => [...]` (§ 2.10.2) covers plain ETH transfers. Vault-style entities (e.g. `eth_vault_evm.cam`) compile cleanly.
- Priority: ~~P0~~ DONE.
- Scheduled: shipped in PLAN_EVM-P0-E.

### 2.11 Multi-file & escape-hatch

#### 2.11.1 Solidity `import` of external files

- Solidity reference: [importing other source files](https://docs.soliditylang.org/en/v0.8.24/layout-of-source-files.html#importing-other-source-files).
- Cambrian status: **done** — two complementary mechanisms ship in
  [PLAN_LIBRARY.md](../PLAN_LIBRARY.md):

  - **Phase 1 — multi-file Cambrian `import "./path.cam"`.** Cross-file
    sharing of `pure fn`, `record`, `enum`, `type`, `extern entity`,
    `event`, `error`, `const`, `library`, and `using` declarations.
    `entity`, `test`, `fuzz`, `invariant` items remain project-owned (any
    occurrence in an imported file is rejected by **V43**). The project
    loader resolves transitive imports relative to each importing file,
    canonicalises paths for cycle detection, and merges duplicates with
    the existing `merge_programs` HashSet checks. Duplicate `import`
    directives in the same file emit **W8**. CLI single-file mode walks
    imports starting from the entry `.cam` — no `project.yaml` required.

  - **Phase 4 — `@solidity_import("...")` annotation on `extern entity`.**
    When present, the EVM emitter (a) prepends `import "<path>";` to the
    generated `.sol`, (b) suppresses its synthetic
    `interface I<name> { ... }` block, and (c) emits call-site casts
    using the verbatim entity name (`IERC20(addr).transfer(...)` instead
    of the synthetic-stub form `IIERC20(addr).transfer(...)`). The
    author chooses the `extern entity` name to match the imported
    `.sol` symbol; mismatches surface at solc compile time. Foundry
    resolves prefixes through `foundry.remappings: [...]` in
    `project.yaml`, which serialises into the generated `foundry.toml`'s
    `remappings = [...]` array (mirrored into every tiered profile).

    ```cambrian
    @solidity_import("@openzeppelin/contracts/token/ERC20/IERC20.sol")
    extern entity IERC20 {
        route transfer(to: address, amount: U256) -> bool;
        view route balanceOf(who: address) -> U256;
    }
    ```

    See `examples/oz-integration/PaymentForwarder.cam` for an end-to-end
    fixture (with foundry remappings) and `cambrian-transpiler/tests/test_solidity_import.rs`
    for the parser-, emitter-, and remappings-level coverage.
- Priority: ~~P1~~ DONE.
- Scheduled: PLAN_LIBRARY.md Phases 1 and 4.

#### 2.11.2 Embedded-Solidity escape hatch

- Cambrian status: **no** — there is no `solidity { ... }` block in the language.
- Priority: P1 — would unlock most remaining P1 items in this list
  (assembly, modifiers) without per-feature surface design.
- Workaround: § 2.11.1 Phase 4 (`@solidity_import("...")`) covers the
  common case (imported OZ types as opaque interfaces). § 2.3.2 (libraries)
  covers reusable pure-fn helpers. Most original motivations for an
  embedded escape hatch are satisfied once those two ship; remaining
  gaps (in-line assembly, modifiers) stay tracked here.
- Scheduled: unscheduled (likely won't ship; superseded by § 2.3.2 / § 2.11.1).

### 2.12 Pragmas / compiler

- See § 1.27 (hardcoded `^0.8.24`, hardcoded SPDX header).

### 2.13 Standards & ecosystem

#### 2.13.1 ERC-165 (`supportsInterface`)

- Solidity reference: [EIP-165](https://eips.ethereum.org/EIPS/eip-165).
- Cambrian status: **partial** — writing the route by hand works end-to-end (`view supportsInterface(interface_id: bytes4) -> bool` compares against the ids it claims), which is what `stdlib/token/ERC7943Min.cam` and `ERC7943.cam` do. What is absent is *derivation*: the compiler does not build the selector table from the entity's own routes, so the id list is maintained by hand and drifts silently if a signature changes.
- Priority: P2 — a hand-written table is correct as long as it is reviewed; the P1 blocker was the missing `bytes4` parameter type, and that is gone.
- Scheduled: unscheduled.

#### 2.13.2 EIP-2612 permit

- Cambrian status: **partial** — implementable manually with `evm::ecrecover` + `hashOf` + `evm::keccak256Packed`. See `examples/uniswap-v2/ERC20.cam`.
- Priority: P0 — every modern ERC-20 ships with permit.
- Workaround: hand-rolled (verbose but works).
- Scheduled: unscheduled (a `permit` macro library would help).

#### 2.13.3 EIP-2535 diamonds

- Cambrian status: **no** (depends on 2.6.2 `delegatecall`, 2.3.1 inheritance, 2.10.2 fallback).
- Priority: P2.
- Scheduled: unscheduled.

#### 2.13.4 EIP-1167 minimal proxies (clones)

- Solidity reference: [EIP-1167](https://eips.ethereum.org/EIPS/eip-1167).
- Cambrian status: **no** — `CambrianFactory` uses full `new Entity{salt: ...}(...)` with the entity bytecode, not a minimal-proxy clone ([`solidity/evm/factory.rs`](../cambrian-transpiler/src/codegen/solidity/evm/factory.rs) / [`solidity/evm/entity.rs`](../cambrian-transpiler/src/codegen/solidity/evm/entity.rs)).
- Priority: P1 — clone factories cut deploy gas by ~10×.
- Scheduled: unscheduled.

#### 2.13.5 OpenZeppelin wrappers (Ownable, ReentrancyGuard, AccessControl, UUPS, ERC-4626)

- Cambrian status: **partial** — most are reimplementable in pure Cambrian (the `examples/governor/*.cam` files demonstrate this), but cannot inherit OZ Solidity directly (depends on 2.3.1 / 2.11.1).
- Priority: P0 (Ownable, ReentrancyGuard) / P1 (AccessControl) / P2 (UUPS, ERC-4626).
- Workaround: in-language reimplementation.
- Scheduled: unscheduled.

### 2.14 Upgradeability

#### 2.14.1 Transparent / UUPS / beacon proxies

- Solidity reference: [EIP-1822](https://eips.ethereum.org/EIPS/eip-1822), [EIP-1967](https://eips.ethereum.org/EIPS/eip-1967).
- Cambrian status: **no** — depends on `delegatecall` (2.6.2) and `fallback` (2.10.2).
- Priority: P1.
- Scheduled: unscheduled.

#### 2.14.2 Storage-layout pinning (`__gap`)

- Solidity reference: OZ upgradeable contracts pattern.
- Cambrian status: **no** — storage layout is implicit in the `members` declaration order.
- Priority: P2.
- Scheduled: unscheduled.

#### 2.14.3 `Initializable` pattern

- Cambrian status: **partial** — deterministic mode auto-generates an `initialize(...)` function with `_initialized` guard and `require(msg.sender == _factory, ...)` ([`solidity/evm/factory.rs`](../cambrian-transpiler/src/codegen/solidity/evm/factory.rs) / [`solidity/evm/entity.rs`](../cambrian-transpiler/src/codegen/solidity/evm/entity.rs)). Behaviour is broadly equivalent to OZ `Initializable` for the single-init case; multi-version reinitialisers (`reinitializer(version)`) are not modelled.
- Priority: P2.
- Scheduled: unscheduled.

#### 2.14.4 `immutable` state

- Cambrian status: **yes** — `identity` members lower to Solidity `immutable` ([`solidity/evm/entity.rs`](../cambrian-transpiler/src/codegen/solidity/evm/entity.rs)).
- Priority: n/a (already done).

---

## 3. By-design TVM↔EVM mismatches

These are not bugs and not scheduled to be "fixed". They are the
fundamental semantic differences between the two execution models.
Cited here so future planning sessions don't re-discover them. See the
full discussion in the internal tracker's Semantic Gap Summary.


| Construct                                                                                                                                                                    | Cambrian behaviour on EVM                                                                                                                                                                                                                               | Reference                                                       |
| ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------- |
| `gosh::`* effects (`rawReserve`, `commit`, `exit`, `mintecc`, `burnecc`, `mintshellq`, `selfdestruct`, `decode`, `setcode`, `setCurrentCode`, `resetStorage`, `setWasmHash`) | `// EVM: gosh::name(args) — no-op on EVM` comment ([`solidity/evm/emitter.rs`](../cambrian-transpiler/src/codegen/solidity/evm/emitter.rs)). Should additionally raise validator E01/E02/E06 (PLAN_EVM-9). | TVM-only.                                                       |
| `msg::pubkey`                                                                                                                                                                | Falls through to `_ => None` ([`solidity/core/expr.rs`](../cambrian-transpiler/src/codegen/solidity/core/expr.rs)). Should raise E03.                                                                                                                                             | EVM has no validator pubkey on incoming messages.               |
| `msg::currencies`                                                                                                                                                            | Same as above; should raise E04.                                                                                                                                                                                                                        | TVM extra-currency model has no EVM equivalent.                 |
| `msg::body`, `msg::int`, `msg::ext`, `msg::createdAt`, `msg::logicaltime`                                                                                                    | All `_ => None` on EVM ([`solidity/core/expr.rs`](../cambrian-transpiler/src/codegen/solidity/core/expr.rs)).                                                                                                                                                                     | TVM message-context shape is not present in EVM tx context.     |
| `sys::pubkey`, `sys::seqno`                                                                                                                                                  | `_ => None` ([`solidity/core/expr.rs`](../cambrian-transpiler/src/codegen/solidity/core/expr.rs)).                                                                                                                                                                                | TVM-only.                                                       |
| Async bounce semantics                                                                                                                                                       | **Rejected (E26).** `rescue`/`recover` is Acki Nacki-only. No EVM try/catch analogue (see PLAN_EVM-8). | EVM call frames return synchronously; no bounce mailbox.        |
| `setcode` / `setCurrentCode` / `resetStorage` / `gosh::updateCode`                                                                                                           | No-op stub (see § 1.13, § 1.14).                                                                                                                                                                                                                        | EVM has no in-place code replacement (proxy patterns required). |
| `deterministic_addresses` mode                                                                                                                                               | **Mandatory on EVM** (defaults `true`; **F6** rejects `false`, U4-6). Maps to `CREATE2` salt + factory-guarded `initialize()` ([`solidity/evm/factory.rs`](../cambrian-transpiler/src/codegen/solidity/evm/factory.rs) / [`solidity/evm/entity.rs`](../cambrian-transpiler/src/codegen/solidity/evm/entity.rs)). Done.                                                                                                                        | Keeps cross-platform `Entity.address(args)` semantics.          |
| `HashMap.exists(k)`                                                                                                                                                          | Sidecar `mapping(K => bool) <name>_exists` ([`solidity/evm/transform.rs`](../cambrian-transpiler/src/codegen/solidity/evm/transform.rs)). Differs from TVM when a key is explicitly set to zero.                                                                                      | Solidity mappings lack a primitive existence test.              |


Priority: all P2 (by-design).

### 3.7 Invariant Foundry harness (handler-first, U4-4c)

Deterministic EVM projects emit a **handler-first** invariant harness
([`evm_test_codegen.rs`](../cambrian-transpiler/src/codegen/evm_test_codegen.rs)):

1. `new CambrianFactory()` → `new Handler()` (empty ctor) →
   `factory.deploy*(address(_handler), …)` per instance.
2. Leftover `init { … }` pins / forall seeding via `vm.store` when ctor
   does not own the member.
3. `handler.cam_wire(…)` wires SUT handles; `targetContract(address(_handler))`.

**Not** emitted on invariant paths: `new Entity(factoryAddr)` +
`.initialize(handler, …)` (legacy Tier B hybrid). Multi-instance invariants
require distinct `identity` tuples (**I18**); revm handler-first parity is
backlog.

`init { m_x: v }` fills a ctor slot only when the member transform maps the
route parameter into the member (`in ctor(x) => x`), not on constant defaults
(`in ctor(recipient) => 0`).

---

## 4. Cross-target test gaps

Tests, fuzz tests, and invariants written in `.cam` files are emitted
to every backend the project enables. The EVM (Foundry) and revm
(Rust) backends silently skip a few constructs that the Acki Nacki /
container backend handles natively. Authors of cross-target tests
should know which steps will become no-ops on EVM.

### 4.1 `msg.<field>` outside `{sender, value}`

- Codegen site: `[evm_test_codegen.rs:584-588](../cambrian-transpiler/src/codegen/evm_test_codegen.rs)`.
- Behaviour: emits `// msg.<field> -- not directly supported on EVM`.
- Priority: P2 (the field has no EVM analogue anyway).

### 4.2 `sys.<field>` outside the supported set

- Codegen site: `[evm_test_codegen.rs:609-612](../cambrian-transpiler/src/codegen/evm_test_codegen.rs)`.
- Supported on Foundry: `now`, `address`, `chainid`, `coinbase`, `basefee`. Anything else is dropped.
- Priority: P2.

### 4.3 Non-`msg`/`sys` `with { ... }` context

- Codegen site: `[evm_test_codegen.rs:618-621](../cambrian-transpiler/src/codegen/evm_test_codegen.rs)`.
- Behaviour: emits `// <namespace> context -- not supported on EVM`.
- Priority: P2.

### 4.4 `registry Entity { ... }` block

- Codegen sites: `[evm_test_codegen.rs:263](../cambrian-transpiler/src/codegen/evm_test_codegen.rs)`, `[evm_test_codegen.rs:395](../cambrian-transpiler/src/codegen/evm_test_codegen.rs)`; `[evm_revm_test_codegen.rs:878](../cambrian-transpiler/src/codegen/evm_revm_test_codegen.rs)`, `[evm_revm_test_codegen.rs:1017](../cambrian-transpiler/src/codegen/evm_revm_test_codegen.rs)`.
- Behaviour: silently skipped with a `// registry <Entity> -- skipped (TVM-specific)` comment.
- Priority: P2 (registry is for TVM address computation; EVM uses `CREATE2` salts).

### 4.5 TVM-specific `expect effects [...]` entries

- Codegen site: `[evm_test_codegen.rs:858](../cambrian-transpiler/src/codegen/evm_test_codegen.rs)`.
- Behaviour: emits `// expect <effect> -- TVM-specific, skipped on EVM` for any platform-effect element such as `rawReserve`, `commit`, `exit`, `mintecc`, etc.
- Priority: P2 — paired with § 3 row 1.

### 4.6 Dangling `Expect`* steps

- Codegen site: `[evm_revm_test_codegen.rs:892-900](../cambrian-transpiler/src/codegen/evm_revm_test_codegen.rs)`.
- Behaviour: `Expect`* steps that don't follow a `call` are dropped with a `// dangling expect-step ignored` comment.
- Priority: P1 — the validator should reject these instead.

### 4.7 Unresolved field reference in revm assertion lowering

- Codegen site: `[evm_revm_test_codegen.rs:1768](../cambrian-transpiler/src/codegen/evm_revm_test_codegen.rs)`.
- Behaviour: previously emitted `/* unresolved field: ... */ U256::ZERO`, now emits a `compile_error!(...)` so the test crate fails to build instead of silently asserting against zero.
- Priority: ~~P0~~ DONE.
- Scheduled: shipped in PLAN_EVM-P0-A.

---

## 5. Roadmap pointers

Mapping every gap above to a `PLAN_EVM-N` phase if any. The "Source"
column points back to the section number in this document, so a
planner can jump from a phase entry to the underlying gap detail and
back.


| Source   | Gap                                           | Priority         | Scheduled                                                                                                                                                                                                                                                                  |
| -------- | --------------------------------------------- | ---------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| § 1.1    | `EnumVariantWithData` lowering                | ~~P1~~ DONE      | tagged-union (`enum <N>_Tag` + `struct <N>`) shipped in PLAN_EVM-4 J1-J3                                                                                                                                       |
| § 1.2    | `gosh::*` in expression position              | ~~P2~~ DONE      | promoted to validator error E15 in PLAN_EVM-2 K1; action-position E02 warning preserved                                                                                                                        |
| § 1.3    | `Range` outside iterator position             | P2               | unscheduled                                                                                                                                                                                                                                                                |
| § 1.4    | `Closure` as value                            | P2               | PLAN_EVM-12 (item 2)                                                                                                                                                                                       |
| § 1.5    | `gen_expr_hoisted` `_` arm silent miscompile  | ~~P0~~ DONE      | validator E16 + codegen `debug_assert!` (PLAN_EVM-2 K1+K3)                                                                                                                                                     |
| § 1.6    | `gen_expr` `_ => None` silent miscompile      | ~~P0~~ DONE      | validator E15-E21 + reserved compositional fallthrough (PLAN_EVM-2 K1+K2+K3)                                                                                                                                   |
| § 1.7    | Tuple `for` patterns                          | DONE             | PLAN_EVM-12 (item 3 — Batch A)                                                                                                                                                                                |
| § 1.8    | Tuple `.fold` patterns                        | DONE             | PLAN_EVM-12 (item 4 — Batch C)                                                                                                                                                                                |
| § 1.9    | Iterator method-chain prefixes + action-`for` | DONE             | PLAN_EVM-12 (items 1+2 — Batches B+E)                                                                                                                                                                         |
| § 1.10   | Mapping transform "no-op" fallback            | ~~P0~~ DONE      | validator E17 (`is_recognised_hashmap_transform`) + codegen `debug_assert!` (PLAN_EVM-2 K1+K3)                                                                                                                 |
| § 1.11   | `let` `Some` / `Deref` / `None` patterns      | P1               | unscheduled                                                                                                                                                                                                                                                                |
| § 1.12   | Mixed-type tuple destructure in `let`         | ~~P0~~ DONE      | per-slot type inference + validator E23 (PLAN_EVM-P0-A)                                                                                                                                                     |
| § 1.13   | `RouteAction::Effect` no-op                   | ~~P2~~ done      | warnings + no-op shipped via PLAN_EVM-9 (E01/E02/E06)                                                                                                                                                 |
| § 1.14   | `RouteAction::UpdateCode` trait default       | P2               | unscheduled                                                                                                                                                                                                                                                                |
| § 1.15   | `rescue` / `recover`                          | ~~P1~~ **E26**   | Acki Nacki-only (2026-08-20); no EVM try/catch                                                                                                                                                                                                                            |
| § 1.16   | Sparse `evm::*` namespace                     | P0 → P1          | sha256 / ripemd160 / balance / blockhash **done in batch 1** (PLAN_EVM-14)                                                                                                                                                                               |
| § 1.17   | `MatchPattern::EnumVariantWithData`           | ~~P1~~ DONE      | tag dispatch + binding substitution (PLAN_EVM-4 J3)                                                                                                                                                            |
| § 1.18   | `Match` inline-form punt                      | DONE / by design | inline now recognises `EnumVariantWithData` (PLAN_EVM-4 J3); remaining bodies legitimately fall through to the hoisted path                                                                                    |
| § 1.19   | `MethodCall::exists` lowering (all shapes)    | ~~P1~~ done      | full coverage shipped in PLAN_EVM-3 Batch G — where-clauses, return values, route bodies, pure-fn args, nested `m[k1].exists(k2)` via 2-level sidecar, let-aliases inlined at codegen time                  |
| § 1.20   | `Type::Tuple` → `bytes`                       | ~~P0~~ DONE      | validator E18 (storage / param), tuples in return position remain valid via Solidity multi-return (PLAN_EVM-2 K2)                                                                                              |
| § 1.21   | Unknown `Type::Generic` → `bytes`             | ~~P0~~ DONE      | validator E19 — only `Vec` / `HashMap` / `Option` accepted (PLAN_EVM-2 K2)                                                                                                                                     |
| § 1.22   | Unknown `Type::Simple` → `uint256`            | ~~P0~~ DONE      | validator E20 (alias-aware via `resolve_alias_for_validate`) (PLAN_EVM-2 K2)                                                                                                                                   |
| § 1.23   | `for` element type defaults to `uint256`      | P1               | Index / FieldAccess arms **done in batch 1** (PLAN_EVM-14)                                                                                                                                                                                               |
| § 1.24   | `Expr::Cast` to non-primitive silently drops  | ~~P1~~ DONE      | validator E21 (alias-aware) (PLAN_EVM-2 K2)                                                                                                                                                                    |
| § 1.31   | Tagged-union enum lowering                    | ~~P1~~ DONE      | new section documenting the schema + storage trade-off (PLAN_EVM-4 J1-J3)                                                                                                                                      |
| § 1.25   | `infer_let_type_entity` defaults              | P1               | EnumVariant / FieldAccess / Range / Some / sys::origin arms **done in batch 1** (PLAN_EVM-14)                                                                                                                                                            |
| § 1.26   | `from Entity(args)` with `args.len() != 1`    | ~~P1~~ DONE      | validator V33 (deterministic-aware) + codegen `debug_assert!` (PLAN_EVM-6 M2)                                                                                                              |
| § 1.32   | `extern entity` foreign-contract interfaces   | ~~P0~~ DONE      | new grammar / AST + populated interface emission + V30 / V31 / V23 fallback + E22 + Acki Nacki no-op (PLAN_EVM-6 M1)                                                                       |
| § 1.27   | Hardcoded pragma + SPDX                       | P1               | unscheduled                                                                                                                                                                                                                                                                |
| § 2.1.1  | `address payable` distinction                 | P1               | unscheduled                                                                                                                                                                                                                                                                |
| § 2.1.2  | Fixed-size arrays `T[N]`                      | P1               | unscheduled                                                                                                                                                                                                                                                                |
| § 2.1.3  | `bytesN` (1..32)                              | P2               | `bytes4` and `bytes32` done (§ 2.1.3); other widths unscheduled                                                                                                                                                                                                            |
| § 2.1.4  | Custom Solidity `error`                       | ~~P0~~ DONE      | `error` decls + `throw Foo(args)` action + V38/V39 (PLAN_EVM-P0-D)                                                                                                                                           |
| § 2.1.5  | User-defined value types                      | P2               | unscheduled                                                                                                                                                                                                                                                                |
| § 2.1.6  | `unchecked { }` blocks                        | P2               | reclassified P0 → P2: semantics fully covered by `+%` / `-%` / `*%` (lowered through `_wadd` / `_wsub` / `_wmul`); Cambrian has no C-style `for (init; cond; step)` so the motivating Solidity loop-counter idiom doesn't apply.                                          |
| § 2.1.7  | Packed structs (sub-256-bit numerics survive) | ~~P0~~ DONE      | narrow integer types (`u8`/`u16`/`u32`/`u64`/`u128` and signed counterparts) survive into Solidity (PLAN_EVM-P0-B)                                                                            |
| § 2.2.1  | User-defined `modifier`                       | P1               | unscheduled                                                                                                                                                                                                                                                                |
| § 2.2.2  | `payable` on routes / constructors            | ~~P0~~ done      | **done in Phase EVM-15 H4 + H5** (PLAN_EVM-15) — auto-detected from `msg::value` reads; constructors always payable                                                                                    |
| § 2.2.3  | `virtual` / `override`                        | P1               | unscheduled                                                                                                                                                                                                                                                                |
| § 2.2.4  | True Solidity `private`                       | P2               | unscheduled                                                                                                                                                                                                                                                                |
| § 2.3.1  | Contract inheritance                          | P1               | unscheduled                                                                                                                                                                                                                                                                |
| § 2.3.2  | `library` / `using for`                       | ~~P1~~ DONE      | full `library Name { ... }` + `using { fn1, fn2 } for T;` + `using LibName for T;` + multi-file `import "...cam"` ([PLAN_LIBRARY.md](../PLAN_LIBRARY.md) Phases 1-3); validators V44 / V45 / V46 / V47 / V48 + W8                                                          |
| § 2.3.3  | Multiple inheritance                          | P2               | unscheduled                                                                                                                                                                                                                                                                |
| § 2.3.4  | Constructor chaining                          | P1               | unscheduled                                                                                                                                                                                                                                                                |
| § 2.4.1  | `event` / `emit`                              | ~~P0~~ DONE      | `event Name(...)` decl + `emit Name(args)` action + V34/V35/V36 (PLAN_EVM-P0-C)                                                                                                                                       |
| § 2.4.2  | Indexed event parameters                      | ~~P0~~ DONE      | `indexed` modifier on `event` params; topic-count pinned by V36 (PLAN_EVM-P0-C)                                                                                                                                       |
| § 2.4.3  | Anonymous events                              | P2               | unscheduled                                                                                                                                                                                                                                                                |
| § 2.5.1  | `revert("...")` with author string            | P1               | unscheduled                                                                                                                                                                                                                                                                |
| § 2.5.2  | Custom errors (= 2.1.4)                       | ~~P0~~ DONE      | shared with § 2.1.4 (PLAN_EVM-P0-D)                                                                                                                                                                          |
| § 2.5.3  | `assert`                                      | P2               | unscheduled                                                                                                                                                                                                                                                                |
| § 2.6.1  | `assembly` / Yul                              | P1               | unscheduled                                                                                                                                                                                                                                                                |
| § 2.6.2  | `staticcall` / `delegatecall`                 | P1               | unscheduled                                                                                                                                                                                                                                                                |
| § 2.6.3  | `addr.balance` for arbitrary addresses        | ~~P1~~ done      | **done in batch 1** (PLAN_EVM-14)                                                                                                                                                                                                                        |
| § 2.6.4  | `addr.transfer` / `addr.send`                 | P2               | unscheduled                                                                                                                                                                                                                                                                |
| § 2.6.5  | `selfdestruct`                                | P2               | unscheduled                                                                                                                                                                                                                                                                |
| § 2.6.6  | `block.blobbasefee` (EIP-4844)                | ~~P1/P2~~ done   | **done in batch 1** (PLAN_EVM-14)                                                                                                                                                                                                                        |
| § 2.6.7  | `blockhash`                                   | ~~P1~~ done      | **done in batch 1** (PLAN_EVM-14)                                                                                                                                                                                                                        |
| § 2.6.8  | `tx.origin` / `tx.gasprice`                   | ~~P2~~ done      | **done in batch 1** (PLAN_EVM-14)                                                                                                                                                                                                                        |
| § 2.6.9  | `msg.data` / `msg.sig`                        | P1               | unscheduled                                                                                                                                                                                                                                                                |
| § 2.6.10 | `gasprice`                                    | ~~P2~~ done      | **done in batch 1** (PLAN_EVM-14)                                                                                                                                                                                                                        |
| § 2.6.11 | Transient storage (EIP-1153)                  | P1               | unscheduled                                                                                                                                                                                                                                                                |
| § 2.7.1  | `sha256` / `ripemd160` first-class            | ~~P1~~ done      | **done in batch 1** (PLAN_EVM-14)                                                                                                                                                                                                                        |
| § 2.7.2  | EIP-712 helpers                               | P1               | unscheduled                                                                                                                                                                                                                                                                |
| § 2.7.3  | EIP-191 helpers                               | P1               | unscheduled                                                                                                                                                                                                                                                                |
| § 2.8.1  | `while` / `do-while`                          | P1               | unscheduled                                                                                                                                                                                                                                                                |
| § 2.8.2  | `break` / `continue`                          | P1               | unscheduled                                                                                                                                                                                                                                                                |
| § 2.8.3  | Typed `try`/`catch`                           | P1               | **none** on EVM (rescue is E26). Former PLAN_EVM-8 analogue withdrawn 2026-08-20                                                                                                                                                                                           |
| § 2.8.4  | Function pointers                             | P2               | unscheduled                                                                                                                                                                                                                                                                |
| § 2.8.5  | Named returns                                 | P2               | unscheduled                                                                                                                                                                                                                                                                |
| § 2.8.7  | HashMap iteration                             | ~~P1~~ DONE      | `.keys()` / `.collect()` + parallel-array maintenance done in batch 1; `.iter()` / `.values()` / `for (k, v) in m` + chain fusion done in Batch D; ordering guarantees pinned by Batch F shape tests (PLAN_EVM-13). |
| § 2.9.1  | General `abi.encode` / `abi.decode`           | P1               | unscheduled                                                                                                                                                                                                                                                                |
| § 2.9.2  | First-class `bytes4` selectors                | P2               | type done; selector-from-signature arithmetic unscheduled                                                                                                                                                                                                                  |
| § 2.10.1 | `payable` constructor                         | ~~P0~~ done      | **done in Phase EVM-15 H4** (PLAN_EVM-15) — generated constructors are always emitted `payable`                                                                                                        |
| § 2.10.2 | `fallback()` / `receive()`                    | ~~P0~~ DONE      | special route names + V40/V41 + auto-`payable` for `receive` + msg::value-driven `fallback` payability (PLAN_EVM-P0-E)                                                                                           |
| § 2.10.3 | ETH-receiving entities                        | ~~P0~~ DONE      | unblocked by 2.10.2 — vault-style entities now compile cleanly (PLAN_EVM-P0-E)                                                                                                                                   |
| § 2.11.1 | Solidity `import`                             | ~~P1~~ DONE      | multi-file Cambrian `import "...cam"` (Phase 1, V43 + W8) + `@solidity_import("...")` on `extern entity` (Phase 4) + `foundry.remappings: [...]` ([PLAN_LIBRARY.md](../PLAN_LIBRARY.md) Phases 1 and 4)                                                                    |
| § 2.11.2 | Embedded-Solidity escape hatch                | P1               | unscheduled — superseded by § 2.3.2 + § 2.11.1 for the OZ-integration / library use cases; remaining motivation (in-line `assembly`, modifiers) still open                                                                                                                  |
| § 2.13.1 | ERC-165 `supportsInterface`                   | P2               | hand-written `supportsInterface(bytes4)` works; auto-derived table unscheduled                                                                                                                                                                                             |
| § 2.13.2 | EIP-2612 permit                               | P0               | unscheduled (workaround exists)                                                                                                                                                                                                                                            |
| § 2.13.3 | EIP-2535 diamonds                             | P2               | unscheduled                                                                                                                                                                                                                                                                |
| § 2.13.4 | EIP-1167 minimal proxies                      | P1               | unscheduled                                                                                                                                                                                                                                                                |
| § 2.13.5 | OZ wrappers                                   | P0/P1/P2         | unscheduled                                                                                                                                                                                                                                                                |
| § 2.14.1 | Proxy patterns                                | P1               | unscheduled                                                                                                                                                                                                                                                                |
| § 2.14.2 | Storage-layout `__gap`                        | P2               | unscheduled                                                                                                                                                                                                                                                                |
| § 2.14.3 | `Initializable` (multi-version)               | P2               | unscheduled                                                                                                                                                                                                                                                                |
| § 4.6    | Dangling `Expect`* validator gap              | P1               | unscheduled                                                                                                                                                                                                                                                                |
| § 4.7    | Unresolved-field placeholder in revm tests    | ~~P0~~ DONE      | placeholder upgraded to `compile_error!` (PLAN_EVM-P0-A)                                                                                                                                                    |


---

## Notes for future maintenance

- When a tracked phase lands, mark the relevant rows here as
"done" rather than deleting — the document doubles as a regression
checklist.
- Line numbers in this document are stable as of the audit; if a
section drifts more than ~40 lines, refresh the citation but keep
the path so cross-links don't rot.
- The four **P0 silent-miscompile** rows (§ 1.5, § 1.6, § 1.10, § 1.20,
§ 1.21, § 1.22, § 1.12, § 4.7) deserve a single hardening pass — turn
every `/* unsupported expr */ 0` and `// no-op` into a hard codegen
error guarded by a validator rule. This is the highest-leverage
cleanup we can do before adding more features.

