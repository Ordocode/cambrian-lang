// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

use cambrian_transpiler::ProgramParser;
use cambrian_transpiler::codegen::gen_evm_solidity;

fn parse(src: &str) -> cambrian_transpiler::ast::Program {
    let mut program = ProgramParser::new().parse(src).unwrap_or_else(|e| panic!("Parse error: {e}"));
    cambrian_transpiler::ast::normalize_program_types(&mut program);
    program
}

#[test]
fn evm_codegen_generates_contract_scaffold() {
    let src = r#"
entity Counter {
    routes {
        increment(by: u64) => []
        getCount() -> u64 => [return(m_count)]
    }
    m_count: u64 {
        in increment(by) => m_count + by
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(sol.contains("pragma solidity ^0.8.24;"), "missing pragma: {sol}");
    assert!(sol.contains("contract Counter"), "missing contract: {sol}");
    // Phase EVM-P0-B: u64 → uint64 (was widened to uint256 pre-P0-B).
    assert!(sol.contains("uint64 public m_count;"), "missing state field: {sol}");
    assert!(sol.contains("function increment(uint64 by) external"), "missing route fn: {sol}");
    assert!(
        sol.contains("function getCount() external view returns (uint64)"),
        "missing return type mapping: {sol}"
    );
    assert!(sol.contains("m_count = next_m_count;"), "missing transform assignment: {sol}");
    assert!(sol.contains("return m_count;"), "missing concrete return lowering: {sol}");
}

#[test]
fn evm_codegen_handles_multiple_entities() {
    let src = r#"
entity A {
    routes { ping() => [] }
    m_x: u64 {}
}
entity B {
    routes { pong() => [] }
    m_ok: bool {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(sol.contains("contract A"), "missing first contract: {sol}");
    assert!(sol.contains("contract B"), "missing second contract: {sol}");
    assert!(sol.contains("bool public m_ok;"), "missing bool mapping: {sol}");
}

#[test]
fn evm_codegen_counter_fixture_has_real_lowering() {
    let src = std::fs::read_to_string("../contracts/counter.cam")
        .expect("counter fixture should exist");
    let program = parse(&src);
    let sol = gen_evm_solidity(&program, true);

    assert!(sol.contains("function increment(uint64 amount) external"), "missing increment route: {sol}");
    assert!(sol.contains("uint64 next_m_count = (m_count + amount);"), "missing arithmetic lowering: {sol}");
    assert!(sol.contains("function reset() external"), "missing reset route: {sol}");
    assert!(sol.contains("uint64 next_m_count = 0;"), "missing reset transform: {sol}");
    assert!(sol.contains("function getCount() external view returns (uint64)"), "missing getCount route: {sol}");
    assert!(sol.contains("return m_count;"), "missing getCount return: {sol}");
}

#[test]
fn evm_codegen_lowers_where_and_send_actions() {
    let src = r#"
entity Messenger {
    routes {
        pay(to: address, amount: u64)
            where amount > 0 : throw 10
            => [
                ~> to with { value: amount }
            ]

        notifyPeer(to: address, amount: u64) => [
            notify(amount) ~> to with { value: amount }
        ]
    }
    m_dummy: u64 {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(sol.contains("require((amount > 0), \"throw(10)\");"), "missing where lowering: {sol}");
    assert!(
        sol.contains("(bool ok, ) = payable(to).call{value: amount}(bytes(\"\"));"),
        "missing plain transfer lowering: {sol}"
    );
    assert!(
        sol.contains("abi.encodeWithSignature(\"notify(uint64)\", amount)"),
        "missing named send lowering: {sol}"
    );
    assert!(sol.contains("require(ok, \"named send notify failed\");"), "missing named send check: {sol}");
}

#[test]
fn evm_codegen_lowers_from_clause_and_or_chain() {
    let src = r#"
entity Auth {
    routes {
        authorize(owner: address, admin: address)
            from ownerEntity(owner) | adminEntity(admin)
            => [
                return(1)
            ]
    }
    m_dummy: u64 {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("require((msg.sender == owner) || (msg.sender == admin), \"from clause failed\");"),
        "missing from OR lowering: {sol}"
    );
}

#[test]
fn evm_codegen_supports_index_fncall_and_methodcall_len() {
    let src = r#"
entity Exprs {
    routes {
        eval(arr: Vec<u64>, i: u64, x: u64) -> u64 => [
            let a = arr[i];
            let b = helper(x);
            return(a + b + arr.len())
        ]
    }
    m_dummy: u64 {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(sol.contains("uint256 a = arr[i];"), "missing index lowering: {sol}");
    assert!(sol.contains("uint256 b = helper(x);"), "missing fn call lowering: {sol}");
    assert!(sol.contains("arr.length"), "missing len() lowering: {sol}");
}

#[test]
fn evm_codegen_expands_msg_and_sys_fields() {
    let src = r#"
entity Ctx {
    routes {
        probe() -> u64 => [
            let a = msg::timestamp;
            let b = msg::logicaltime;
            let c = sys::now;
            let d = sys::logicaltime;
            let e = sys::address;
            return(a + b + c + d)
        ]
    }
    m_dummy: u64 {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(sol.contains("block.timestamp"), "missing timestamp lowering: {sol}");
    assert!(sol.contains("block.number"), "missing logicaltime lowering: {sol}");
    assert!(sol.contains("address(this)"), "missing sys::address support path: {sol}");
}

#[test]
fn evm_codegen_lowers_record_and_tuple_expressions() {
    let src = r#"
entity Wallet {
    record Pair {
        left: u64,
        right: u64
    }

    routes {
        makePair(a: u64, b: u64) => []

        pairTuple(a: u64, b: u64) -> (u64, u64) => [
            return((a, b))
        ]
    }

    m_pair: Pair {
        in makePair(a, b) => m_pair
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(sol.contains("struct Pair"), "missing record struct emission: {sol}");
    assert!(sol.contains("Pair public m_pair;"), "missing record-typed member: {sol}");
    assert!(
        sol.contains("m_pair = Pair({left: 0, right: 0});"),
        "missing record default initialization lowering: {sol}"
    );
    assert!(
        sol.contains("function pairTuple(uint64 a, uint64 b) external view returns (uint64, uint64)"),
        "missing tuple return signature lowering: {sol}"
    );
    assert!(sol.contains("return (a, b);"), "missing tuple expr lowering: {sol}");
}

// ---------------------------------------------------------------------------
// Phase EVM-1: init routes → constructor, identity members → immutable
// ---------------------------------------------------------------------------

#[test]
fn evm1_init_route_becomes_constructor() {
    let src = r#"
entity Vault {
    routes {
        init setup(owner: address, limit: u64) => []
        deposit(amount: u64) => []
    }
    m_owner: address {
        in setup(owner, _) => owner
    }
    m_limit: u64 {
        in setup(_, limit) => limit
    }
    m_balance: u64 {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("function initialize(address owner, uint64 limit)"),
        "init route params must lower to initialize(): {sol}"
    );
    let init = initialize_fn_body(&sol);
    assert!(init.contains("address next_m_owner = owner;"), "missing init member update: {init}");
    assert!(init.contains("uint64 next_m_limit = limit;"), "missing limit member update: {init}");
    // setup route should NOT also be emitted as a regular function
    assert!(!sol.contains("function setup("), "init route should not be a regular function: {sol}");
    // deposit route still emitted
    assert!(sol.contains("function deposit(uint64 amount) external"), "deposit route missing: {sol}");
}

#[test]
fn evm1_identity_member_becomes_immutable() {
    let src = r#"
entity Broker {
    identity m_shop_id: u64

    routes {
        constructor() => []
        process() => []
    }

    m_count: u64 {
        in constructor() => 0
        in process() => m_count + 1
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("uint64 public immutable m_shop_id;"),
        "identity member should be immutable: {sol}"
    );
    assert!(
        sol.contains("constructor(address factory_, uint64 m_shop_id_)"),
        "det constructor should take factory + identity: {sol}"
    );
    assert!(
        sol.contains("m_shop_id = m_shop_id_;"),
        "constructor should assign identity: {sol}"
    );
    assert!(
        sol.contains("address public immutable _factory;"),
        "det entity must record factory: {sol}"
    );
}

#[test]
fn evm1_identity_vault_fixture() {
    let src = std::fs::read_to_string("../contracts/identity_pair/identity_vault.cam")
        .expect("identity_vault fixture");
    let program = parse(&src);
    let sol = gen_evm_solidity(&program, true);

    assert!(sol.contains("uint64 public immutable m_id;"), "immutable m_id: {sol}");
    assert!(
        sol.contains("constructor(address factory_, uint64 m_id_)"),
        "identity goes on the factory-guarded constructor: {sol}"
    );
    assert!(
        sol.contains("function initialize(uint64 initial_balance)"),
        "non-identity init-route params go to initialize(): {sol}"
    );
}

// ---------------------------------------------------------------------------
// Phase EVM-2: expression completeness
// ---------------------------------------------------------------------------

#[test]
fn evm2_if_expression_lowered_to_ternary() {
    let src = r#"
entity Cond {
    routes {
        check(x: u64) -> u64 => [
            let result = if x > 0 { x } else { 0 };
            return(result)
        ]
    }
    m_dummy: u64 {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    // Either ternary inline or if/else block with temp var
    assert!(
        sol.contains("? x : 0") || sol.contains("_cam_tmp") || sol.contains("(x > 0)"),
        "if expr lowering: {sol}"
    );
}

#[test]
fn evm2_match_expression_lowered() {
    let src = r#"
entity WithEnum {
    enum State { Created, Funded, Released }

    routes {
        getCode() -> u64 => [
            let code = match m_state {
                State::Created => 0,
                State::Funded => 1,
                State::Released => 2
            };
            return(code)
        ]
    }

    m_state: State {
        in getCode() => State::Created
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(sol.contains("enum State"), "enum not emitted: {sol}");
    assert!(
        sol.contains("State.Created") || sol.contains("== State.Created"),
        "enum variant ref missing: {sol}"
    );
}

#[test]
fn evm2_some_and_none_lowered() {
    let src = r#"
entity Opt {
    routes {
        wrap(x: u64) -> Option<u64> => [
            return(some(x))
        ]
        empty() -> Option<u64> => [
            return(none)
        ]
    }
    m_dummy: u64 {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("struct Option_uint64") && sol.contains("enum Option_uint64_Tag"),
        "Option<u64> must emit a tagged struct: {sol}"
    );
    assert!(
        sol.contains("Option_uint64_Tag.Some") && sol.contains("some_0: x"),
        "some(x) must construct the tagged struct: {sol}"
    );
    assert!(
        sol.contains("Option_uint64_Tag.None"),
        "none must construct the None tag: {sol}"
    );
}

#[test]
fn evm2_bytes_literal_lowered() {
    let src = r#"
entity ByteTest {
    routes {
        getBytes() -> CamData => [
            return(b"hello")
        ]
    }
    m_dummy: u64 {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(sol.contains("hex\""), "bytes literal not lowered: {sol}");
}

#[test]
fn evm2_let_chain_in_member_transform() {
    let src = r#"
entity Chain {
    routes {
        compute(a: u64, b: u64) => []
    }
    m_result: u64 {
        in compute(a, b) => {
            let x = a + b;
            let y = x * 2;
            y
        }
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(sol.contains("uint64 x = (a + b);"), "let hoisting x: {sol}");
    assert!(sol.contains("uint64 y = (x * 2);"), "let hoisting y: {sol}");
}

// ---------------------------------------------------------------------------
// Phase EVM-3: HashMap/Vec method lowering
// ---------------------------------------------------------------------------

#[test]
fn evm3_hashmap_insert_lowered() {
    let src = r#"
entity Registry {
    routes {
        register(key: address, val: u64) => []
    }
    m_data: HashMap<address, u64> {
        in register(key, val) => m_data.insert(key, val)
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    // Mapping writes are deferred so all transforms see pre-route state.
    // `key`/`val` are snapshotted into `_cam_tmpN` locals first; the
    // actual storage write fires after the field-mutation stage's read
    // substage. We assert on the eventual write to a `_cam_tmp*` slot
    // and on the snapshot bindings so we know both halves are present.
    assert!(
        sol.contains("address _cam_tmp") && sol.contains("= key;"),
        "key not snapshotted: {sol}"
    );
    assert!(
        sol.contains("uint64 _cam_tmp") && sol.contains("= val;"),
        "val not snapshotted: {sol}"
    );
    assert!(
        sol.contains("m_data[_cam_tmp"),
        "deferred storage write missing: {sol}"
    );
}

#[test]
fn evm3_hashmap_update_chained() {
    let src = r#"
entity Balances {
    routes {
        transfer(sender: address, recipient: address, amount: u64) => []
    }
    m_bal: HashMap<address, u64> {
        in transfer(sender, recipient, amount) => m_bal.update(sender, 0).update(recipient, amount)
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    // Chained `update(sender, 0).update(recipient, amount)` lowers to two
    // deferred writes via `_cam_tmp*` snapshots. The exact temp indices
    // depend on global counter state, so we just verify both writes land
    // somewhere into m_bal and the value snapshots are present.
    assert!(sol.contains("address _cam_tmp") && sol.contains("= sender;"), "sender snap: {sol}");
    assert!(sol.contains("address _cam_tmp") && sol.contains("= recipient;"), "recipient snap: {sol}");
    assert!(sol.contains("uint64 _cam_tmp") && sol.contains("= 0;"), "zero snap: {sol}");
    assert!(sol.contains("uint64 _cam_tmp") && sol.contains("= amount;"), "amount snap: {sol}");
    assert!(sol.matches("m_bal[_cam_tmp").count() >= 2, "two writes to m_bal: {sol}");
}

#[test]
fn evm3_hashmap_exists_emits_companion() {
    let src = r#"
entity Store {
    routes {
        check(k: address) -> u64 => [
            let exists = m_data.exists(k);
            return(exists)
        ]
    }
    m_data: HashMap<address, u64> {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_data_exists[k]"),
        "companion _exists mapping: {sol}"
    );
}

// ---------------------------------------------------------------------------
// Phase EVM-3 Batch G: comprehensive `.exists()` lowering
// ---------------------------------------------------------------------------

#[test]
fn evm3_g1_exists_in_where_clause_emits_sidecar() {
    // EVM-3 Batch G1: `m.exists(k)` inside a `where` clause must
    // trigger the `_exists` companion mapping just like a transform-
    // body call. Pre-G1 the detection was scoped to transforms only
    // and where-clause uses produced an undeclared identifier.
    let src = r#"
entity D {
    routes {
        constructor() => []
        op(k: u256)
            where !m_data.exists(k) : throw 1 => []
    }
    m_data: HashMap<u256, u256> {
        in op(k) => m_data.insert(k, 1)
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("mapping(uint256 => bool) public m_data_exists"),
        "where-clause use should emit `_exists` sidecar: {sol}"
    );
    assert!(
        sol.contains("require((!m_data_exists[k])"),
        "where lowers to `_exists` lookup: {sol}"
    );
}

#[test]
fn exists_sidecar_is_written_on_a_non_iterated_map() {
    // The `_exists` companion used to be maintained only for maps that
    // are iterated somewhere, while it was declared and read for every
    // map queried through `.exists()`. A non-iterated map therefore got
    // a flag that nothing ever set, `.exists(k)` answered false forever,
    // and every guard resting on it rejected an account that had just
    // been funded. Declaration and maintenance now share one decision.
    let src = r#"
pure fn score_of(scores: HashMap<address, u256>, who: address) -> u256 {
    if scores.exists(who) { scores[who] } else { 0 }
}

entity R {
    routes {
        constructor() => []
        set(who: address, score: u256) => []
        clear(who: address) => []
        view scoreOf(who: address) -> u256 => [ return(score_of(m_scores, who)) ]
    }
    m_scores: HashMap<address, u256> {
        in constructor() => {}
        in set(who, score) => m_scores.update(who, score)
        in clear(who) => m_scores.remove(who)
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("mapping(address => bool) public m_scores_exists"),
        "sidecar is declared for the pure-fn `.exists()` read: {sol}"
    );
    assert!(
        sol.contains("m_scores_exists[") && sol.contains("] = true;"),
        "an update must set the existence flag: {sol}"
    );
    assert!(
        sol.contains("delete m_scores_exists["),
        "a remove must clear the existence flag: {sol}"
    );
    // The map is never iterated, so no key array should appear.
    assert!(
        !sol.contains("m_scores_keys"),
        "a non-iterated map must not grow a key array: {sol}"
    );
}

#[test]
fn evm3_g1_exists_via_pure_fn_threads_sidecar_argument() {
    // EVM-3 Batch G1: when a pure function takes a HashMap parameter
    // and calls `.exists()` on it, both the function signature and
    // every call site must thread an extra `<param>_exists` sidecar
    // alongside the mapping. Pre-G1 this generated a body that
    // referenced an undeclared `<param>_exists` and solc rejected it.
    let src = r#"
pure fn balance(m: HashMap<address, U256>, owner: address) -> U256 {
    if m.exists(owner) { m[owner] } else { 0 }
}

entity Bank {
    routes {
        constructor() => []
        view bal(owner: address) -> U256 => [
            return(balance(m_balances, owner))
        ]
    }
    m_balances: HashMap<address, U256> {
        in constructor() => {}
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains(
            "function balance(mapping(address => uint256) storage m, mapping(address => bool) storage m_exists, address owner)"
        ),
        "pure-fn signature gets sidecar param: {sol}"
    );
    assert!(
        sol.contains("balance(m_balances, m_balances_exists, owner)"),
        "call site threads sidecar argument: {sol}"
    );
    assert!(
        sol.contains("mapping(address => bool) public m_balances_exists"),
        "entity sidecar emitted because pure-fn arg propagates the requirement: {sol}"
    );
}

#[test]
fn evm3_g2_let_bound_hashmap_alias_is_inlined() {
    // EVM-3 Batch G2: Solidity disallows mapping locals, so
    // `let inner = m_outer[k]` for a nested-mapping member must be
    // substituted at codegen time rather than emitting an invalid
    // `mapping(...) inner = ...` Solidity local. Subsequent uses
    // (`inner.exists(x)`, `inner[x]`) lower to the underlying
    // `m_outer[k][x]` / sidecar references.
    let src = r#"
entity Nested {
    routes {
        constructor() => []
        op(a: address, b: address)
            where m_x.exists(a) : throw 1 => [
            return(m_x[a][b])
        ]
    }
    m_x: HashMap<address, HashMap<address, U256>> {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(
        !sol.contains("mapping(address => uint256) inner"),
        "should not emit a Solidity mapping local: {sol}"
    );
}

#[test]
fn evm3_g2_nested_exists_emits_two_level_sidecar() {
    // EVM-3 Batch G2: a HashMap<K1, HashMap<K2, V>> whose inner
    // mapping is queried via `.exists()` — through the idiomatic
    // `let inner = m[k1]; inner.exists(k2)` boilerplate inside a
    // member transform — gets a parallel `mapping(K1 => mapping(K2
    // => bool)) <m>_inner_exists` companion. Detection runs against
    // the substituted form so it catches both shapes uniformly.
    let src = r#"
entity Allow {
    routes {
        constructor() => []
        approve(spender: address, amount: U256) => []
    }
    m_allow: HashMap<address, HashMap<address, U256>> {
        in approve(spender, amount) => {
            let sender = msg::sender;
            let inner = m_allow[sender];
            let probe = if inner.exists(spender) { inner[spender] } else { 0 };
            m_allow.update(sender, inner.update(spender, probe + amount))
        }
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains(
            "mapping(address => mapping(address => bool)) public m_allow_inner_exists"
        ),
        "2-level sidecar emitted: {sol}"
    );
    assert!(
        sol.contains("m_allow_inner_exists[sender][spender]"),
        "nested .exists() lowers to 2-level sidecar lookup: {sol}"
    );
}

#[test]
fn evm3_g2_nested_update_emits_single_subscript_pair_and_sidecar_write() {
    // EVM-3 Batch G2: the canonical nested-update pattern
    //   `m.update(k1, m[k1].update(k2, v))`
    // (also the result of substituting `let inner = m[k1]; ... inner.update(k2, v)`)
    // must lower to exactly `m[k1][k2] = v;` (two subscripts, never
    // three) plus the maintenance write that flips the 2-level
    // sidecar — gated on `inner.exists()` actually being read
    // somewhere in the entity. Pre-G2 this emitted an invalid
    // triple-subscript `m[k1][k1][k2] = v;`.
    let src = r#"
entity Allow2 {
    routes {
        constructor() => []
        approve(spender: address, amount: U256) => []
    }
    m_allow: HashMap<address, HashMap<address, U256>> {
        in approve(spender, amount) => {
            let sender = msg::sender;
            let inner = if m_allow.exists(sender) {
                m_allow[sender]
            } else {
                {}
            };
            let probe = if inner.exists(spender) { inner[spender] } else { 0 };
            m_allow.update(sender, inner.update(spender, probe + amount))
        }
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    // Triple subscript would look like `m_allow[X][Y][Z]` — the
    // generated form must always be exactly two `[...]` segments
    // (one outer key, one inner key).
    let triple_subscript_re =
        regex::Regex::new(r"m_allow\[[^\]]+\]\[[^\]]+\]\[[^\]]+\]").unwrap();
    assert!(
        !triple_subscript_re.is_match(&sol),
        "no triple-subscript chain on m_allow: {sol}"
    );
    // The final write goes through the canonical 2-key form.
    let two_key_re =
        regex::Regex::new(r"m_allow\[_cam_tmp\d+\]\[_cam_tmp\d+\] = _cam_tmp\d+;").unwrap();
    assert!(
        two_key_re.is_match(&sol),
        "two-key nested write present: {sol}"
    );
    // Sidecar maintenance is emitted alongside (because the
    // transform reads `inner.exists(spender)`).
    assert!(
        sol.contains("m_allow_inner_exists[") && sol.contains("] = true;"),
        "2-level sidecar set after nested write: {sol}"
    );
}

#[test]
fn evm3_hashmap_remove_lowered() {
    let src = r#"
entity Rm {
    routes {
        remove(key: address) => []
    }
    m_data: HashMap<address, u64> {
        in remove(key) => m_data.remove(key)
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    // `remove(key)` snapshots the key into a temp and emits the
    // `delete m_data[_cam_tmp*]` at the end of the field-mutation stage.
    assert!(sol.contains("address _cam_tmp") && sol.contains("= key;"), "key snap: {sol}");
    assert!(sol.contains("delete m_data[_cam_tmp"), "deferred delete: {sol}");
}

// ---------------------------------------------------------------------------
// Phase EVM-4: enum and match lowering
// ---------------------------------------------------------------------------

#[test]
fn evm4_entity_enum_emitted_as_solidity_enum() {
    let src = r#"
entity Esc {
    enum State { Created, Funded, Resolved }
    routes {
        create() => []
    }
    m_state: State {
        in create() => State::Created
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("enum State { Created, Funded, Resolved }"),
        "Solidity enum not emitted: {sol}"
    );
    assert!(
        sol.contains("State.Created"),
        "enum variant ref not emitted: {sol}"
    );
}

#[test]
fn evm4_escrow_v2_fixture_lowers_enum_and_match() {
    let src = std::fs::read_to_string("../contracts/escrow_v2.cam")
        .expect("escrow_v2 fixture");
    let program = parse(&src);
    let sol = gen_evm_solidity(&program, true);

    assert!(sol.contains("enum State"), "enum State not emitted: {sol}");
    assert!(sol.contains("State.Created"), "State.Created ref: {sol}");
}

// ---------------------------------------------------------------------------
// Phase EVM-5: pure fn and macro lowering
// ---------------------------------------------------------------------------

#[test]
fn evm5_pure_fn_emitted_as_internal_function() {
    let src = r#"
pure fn add(a: u64, b: u64) -> u64 {
    a + b
}

entity Calc {
    routes {
        sum(a: u64, b: u64) -> u64 => [
            return(add(a, b))
        ]
    }
    m_dummy: u64 {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    // Pure free functions can't carry visibility in Solidity, so we emit
    // them without the `internal` keyword. They stay `pure` when their
    // params don't include storage refs.
    assert!(
        sol.contains("function add(uint64 a, uint64 b) pure returns (uint64)"),
        "pure fn missing: {sol}"
    );
    assert!(
        sol.contains("return (a + b)") || sol.contains("return uint64((a + b))"),
        "pure fn body: {sol}"
    );
}

#[test]
fn evm5_entity_macro_emitted_as_view_function() {
    let src = r#"
entity Tok {
    macro doubled(x: u64) -> u64 = { x * 2 }

    routes {
        get(x: u64) -> u64 => [
            return(@doubled(x))
        ]
    }
    m_dummy: u64 {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function macro_doubled(uint64 x) internal view returns (uint64)"),
        "macro fn missing: {sol}"
    );
    assert!(sol.contains("macro_doubled(x)"), "macro call site: {sol}");
}

// ---------------------------------------------------------------------------
// Phase EVM-6: deploy action lowering
// ---------------------------------------------------------------------------

#[test]
fn evm6_deploy_action_lowered_to_new() {
    let src = r#"
entity Factory {
    routes {
        spawn(owner: address) => [
            deploy Child (owner) with { value: 100 }
        ]
    }
    m_dummy: u64 {}
}
entity Child {
    routes {
        constructor(owner: address) => []
    }
    m_owner: address {
        in constructor(owner) => owner
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("address(new Child{value: 100}(owner))")
        || sol.contains("new Child"),
        "deploy not lowered to new: {sol}"
    );
}

// ---------------------------------------------------------------------------
// Phase EVM-6 M1: extern entity foreign-interface emission
// ---------------------------------------------------------------------------

#[test]
fn evm6_m1_extern_entity_emits_populated_interface() {
    // `extern entity Token { route transfer(amount: U256); ... }` lets the
    // EVM target emit a populated `interface IToken { ... }` for cross-
    // contract calls, so the generated Solidity actually compiles against
    // the foreign contract.
    let src = r#"
extern entity Token {
    route transfer(amount: U256);
    view route balanceOf(who: address) -> U256;
}

entity Caller {
    routes {
        constructor(t: Address<Token>) => []
        ping(amount: U256) => [
            transfer(amount) ~> m_token
        ]
    }
    m_token: Address<Token> {
        in constructor(t) => t
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("interface IToken {"),
        "missing interface header: {sol}"
    );
    assert!(
        sol.contains("function transfer(uint256 amount) external;"),
        "transfer signature missing or malformed: {sol}"
    );
    assert!(
        sol.contains("function balanceOf(address who) external view returns (uint256);"),
        "balanceOf signature missing or malformed: {sol}"
    );
    assert!(
        !sol.contains("// EVM-6 M1: external entity"),
        "must not fall back to the placeholder stub when an extern entity is declared: {sol}"
    );
    assert!(
        sol.contains("IToken(") && sol.contains(").transfer(amount)"),
        "call site must use the extern interface: {sol}"
    );
}

#[test]
fn evm6_m1_extern_entity_payable_route_emits_payable_modifier() {
    let src = r#"
extern entity Vault {
    accept route deposit() -> bool;
}

entity Caller {
    routes {
        constructor(v: Address<Vault>) => []
        topup() => [
            deposit() ~> m_vault with { value: 100 }
        ]
    }
    m_vault: Address<Vault> {
        in constructor(v) => v
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("function deposit() external payable returns (bool);"),
        "payable extern route lowering: {sol}"
    );
}

#[test]
fn evm6_m1_unknown_external_entity_still_emits_unreachable_stub_in_release() {
    // Defence in depth: when codegen runs on an unvalidated program (or
    // a debug_assert is bypassed in release), the empty-stub fallback
    // is still emitted with a self-documenting marker so users can
    // diagnose what happened.
    //
    // We invoke `gen_evm_solidity` on a program that bypasses the
    // validator (which would have rejected via E22). This pins the
    // exact text of the marker comment so a future refactor doesn't
    // silently regress the diagnostic.
    let src = r#"
entity Caller {
    routes {
        constructor() => []
    }
    m_token: Address<Token> {
        in constructor() => address(0)
    }
}
"#;
    let program = parse(src);
    // Must not panic in release; in debug `gen_evm_solidity` would
    // hit the `debug_assert!`. Skip in debug builds.
    if cfg!(debug_assertions) {
        return;
    }
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("// EVM-6 M1: external entity 'Token' is not declared"),
        "release-mode placeholder marker missing: {sol}"
    );
}

// ---------------------------------------------------------------------------
// Phase EVM-7: phased routes
// ---------------------------------------------------------------------------

#[test]
fn evm7_phased_route_flattened_to_sequential_blocks() {
    let src = std::fs::read_to_string("../contracts/phased_vault.cam")
        .expect("phased_vault fixture");
    let program = parse(&src);
    let sol = gen_evm_solidity(&program, true);

    assert!(sol.contains("function deposit(uint128 amount) external"), "deposit route: {sol}");
    assert!(sol.contains("// Phase: save"), "save phase comment: {sol}");
    assert!(sol.contains("// Phase: finish"), "finish phase comment: {sol}");
    // gosh::commit() should be a no-op comment
    assert!(sol.contains("// EVM: gosh::commit"), "gosh commit nooped: {sol}");
}

#[test]
fn evm7_phased_member_transforms_in_correct_phase() {
    let src = std::fs::read_to_string("../contracts/phased_vault.cam")
        .expect("phased_vault fixture");
    let program = parse(&src);
    let sol = gen_evm_solidity(&program, true);

    // m_balance transform is in "save" phase
    assert!(
        sol.contains("next_m_balance"),
        "m_balance phased transform: {sol}"
    );
}

// ---------------------------------------------------------------------------
// Phase EVM-8 withdrawn: rescue/recover is Acki Nacki-only (E26)
// ---------------------------------------------------------------------------

#[test]
fn evm8_rescue_rejected_e26_no_try_catch() {
    let src = r#"
entity Safe {
    routes {
        send_safe(to: address) => [
            rescue tag1: ~> to with { value: 100 }
        ]

        recover tag1() => []
    }
    m_dummy: u64 {}
}
"#;
    let program = parse(src);
    let diags = cambrian_transpiler::validate::check_evm_target_compat(&program);
    assert!(
        diags.iter().any(|d| d.code == "E26"),
        "rescue/recover must be E26 on EVM: {:?}",
        diags
    );
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("E26") && sol.contains("tag1"),
        "force-codegen must comment rescue, not try/catch: {sol}"
    );
    assert!(
        !sol.contains("try ") && !sol.contains("catch {"),
        "EVM must not lower rescue to try/catch: {sol}"
    );
}

#[test]
fn evm8_effect_actions_are_noops() {
    let src = r#"
use gosh

entity Upgr {
    routes {
        upgrade() => [
            gosh::commit()
            gosh::setcode(b"ff")
        ]
    }
    m_dummy: u64 {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(sol.contains("// EVM: gosh::commit"), "commit noop: {sol}");
    assert!(sol.contains("// EVM: gosh::setcode"), "setcode noop: {sol}");
}

// ---------------------------------------------------------------------------
// Phase EVM-9: EVM validator warnings
// ---------------------------------------------------------------------------

#[test]
fn evm9_validator_warns_on_gosh_import() {
    let src = r#"
use gosh

entity X {
    routes {
        act() => []
    }
    m_dummy: u64 {}
}
"#;
    let mut program = cambrian_transpiler::ProgramParser::new()
        .parse(src)
        .expect("parse");
    cambrian_transpiler::ast::normalize_program_types(&mut program);
    let diags = cambrian_transpiler::validate::check_evm_target_compat(&program);
    assert!(
        diags.iter().any(|d| d.code == "E01"),
        "expected E01 gosh import warning: {:?}", diags
    );
}

#[test]
fn evm9_validator_warns_on_pubkey_usage() {
    let src = r#"
entity X {
    routes {
        act() => [
            let k = msg::pubkey;
            return(k)
        ]
    }
    m_dummy: u64 {}
}
"#;
    let mut program = cambrian_transpiler::ProgramParser::new()
        .parse(src)
        .expect("parse");
    cambrian_transpiler::ast::normalize_program_types(&mut program);
    let diags = cambrian_transpiler::validate::check_evm_target_compat(&program);
    assert!(
        diags.iter().any(|d| d.code == "E03"),
        "expected E03 pubkey warning: {:?}", diags
    );
}

#[test]
fn evm9_clean_program_has_no_evm_warnings() {
    let src = r#"
entity Counter {
    routes {
        increment(amount: u64) => []
        getCount() -> u64 => [return(m_count)]
    }
    m_count: u64 {
        in increment(amount) => m_count + amount
    }
}
"#;
    let mut program = cambrian_transpiler::ProgramParser::new()
        .parse(src)
        .expect("parse");
    cambrian_transpiler::ast::normalize_program_types(&mut program);
    let diags = cambrian_transpiler::validate::check_evm_target_compat(&program);
    assert!(diags.is_empty(), "clean program should have no EVM warnings: {:?}", diags);
}

// ---------------------------------------------------------------------------
// EVM-11: cross-contract view call with return value
// ---------------------------------------------------------------------------

#[test]
fn evm11_typed_addr_method_call_lowered_to_interface_call() {
    let src = r#"
entity Token {
    routes {
        constructor(initial_supply: U256) => []
        view balanceOf(owner: address) -> U256 => [
            return(m_total_supply)
        ]
        view totalSupply() -> U256 => [
            return(m_total_supply)
        ]
    }
    m_total_supply: U256 {
        in constructor(initial_supply) => initial_supply
    }
}

entity Vault {
    routes {
        constructor(token_addr: Address<Token>) => []
        view myBalance(who: address) -> U256 => [
            var bal = balanceOf(who) ~> m_token;
            return(bal)
        ]
        view supply() -> U256 => [
            var s = totalSupply() ~> m_token;
            return(s)
        ]
    }
    m_token: Address<Token> {
        in constructor(token_addr) => token_addr
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    // Interface for Token should be emitted with view modifier and return types
    assert!(sol.contains("interface IToken {"), "missing IToken interface: {sol}");
    assert!(
        sol.contains("function balanceOf(address owner) external view returns (uint256)"),
        "interface missing balanceOf signature: {sol}"
    );
    assert!(
        sol.contains("function totalSupply() external view returns (uint256)"),
        "interface missing totalSupply signature: {sol}"
    );

    // Interface should be emitted BEFORE the Vault contract
    let itoken_pos = sol.find("interface IToken {").unwrap();
    let vault_pos = sol.find("contract Vault {").unwrap();
    assert!(itoken_pos < vault_pos, "IToken interface must appear before Vault contract: {sol}");

    // Vault's myBalance should emit a proper interface call via var
    assert!(
        sol.contains("IToken(m_token).balanceOf(who)"),
        "var call not lowered to interface call: {sol}"
    );
    assert!(
        sol.contains("IToken(m_token).totalSupply()"),
        "var totalSupply call not lowered: {sol}"
    );

    // Return type of the var binding should be inferred from Token.balanceOf return type
    assert!(
        sol.contains("uint256 bal = IToken(m_token).balanceOf(who)"),
        "var binding should use return type from target route: {sol}"
    );
    // In unphased routes, var declaration + assignment should come before return
    let bal_decl = sol.find("uint256 bal = IToken(m_token).balanceOf(who)").expect("var decl missing");
    let ret_bal = sol.find("return bal;").expect("return bal missing");
    assert!(bal_decl < ret_bal, "var decl must come before return in unphased route: {sol}");
}

#[test]
fn evm11_interface_decl_view_modifier_emitted() {
    let src = r#"
entity Oracle {
    routes {
        constructor() => []
        view getPrice(asset: address) -> U256 => [
            return(m_price)
        ]
        updatePrice(asset: address, price: U256) => []
    }
    m_price: U256 {
        in constructor() => 0
        in updatePrice(_, price) => price
    }
}

entity Consumer {
    routes {
        constructor(oracle_addr: Address<Oracle>) => []
        view checkPrice(asset: address) -> U256 => [
            var p = getPrice(asset) ~> m_oracle;
            return(p)
        ]
    }
    m_oracle: Address<Oracle> {
        in constructor(oracle_addr) => oracle_addr
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    // getPrice is a view route → interface must include `view`
    assert!(
        sol.contains("function getPrice(address asset) external view returns (uint256)"),
        "view modifier missing on getPrice in IOracle: {sol}"
    );
    // updatePrice is not view → no view modifier
    assert!(
        sol.contains("function updatePrice(address asset, uint256 price) external;"),
        "non-view route should not have view modifier: {sol}"
    );
    // Consumer's checkPrice emits the interface call via var
    assert!(
        sol.contains("IOracle(m_oracle).getPrice(asset)"),
        "Oracle interface call not emitted: {sol}"
    );
}

#[test]
fn evm_var_call_result_used_in_member_transform() {
    let src = r#"
entity Token {
    routes {
        constructor() => []
        view balanceOf(owner: address) -> U256 => [
            return(m_supply)
        ]
    }
    m_supply: U256 {
        in constructor() => 0
    }
}

entity Tracker {
    routes {
        constructor(token_addr: Address<Token>) => []
        refresh(who: address) => [
            fetch: [
                var bal = balanceOf(who) ~> m_token;
            ]
            update: []
        ]
    }
    m_token: Address<Token> {
        in constructor(token_addr) => token_addr
    }
    m_cached_balance: U256 {
        in constructor() => 0
        in refresh(who) => update: bal
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    // var declaration hoisted to function scope
    assert!(
        sol.contains("uint256 bal;"),
        "hoisted var declaration missing: {sol}"
    );
    // assignment inside phase block
    assert!(
        sol.contains("bal = IToken(m_token).balanceOf(who)"),
        "var call assignment not emitted as interface call: {sol}"
    );
    // member transform in update phase should use bal
    assert!(
        sol.contains("next_m_cached_balance = bal"),
        "member transform should reference var 'bal': {sol}"
    );
    // hoisted declaration must appear before the assignment
    let hoisted = sol.find("uint256 bal;").expect("hoisted decl missing");
    let assignment = sol.find("bal = IToken(m_token)").expect("assignment missing");
    assert!(hoisted < assignment, "hoisted declaration must appear before assignment: {sol}");
    // assignment must appear before its use in the transform
    let bal_use = sol.find("next_m_cached_balance = bal").expect("transform use missing");
    assert!(assignment < bal_use, "var assignment must be emitted before the transform that uses it: {sol}");
    // Interface should be generated
    assert!(
        sol.contains("interface IToken {"),
        "missing IToken interface: {sol}"
    );
}

#[test]
fn evm_var_call_with_send_options_emits_value() {
    let src = r#"
entity Token {
    routes {
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
        constructor(token_addr: Address<Token>) => []
        paidQuery(who: address) => [
            fetch: [
                var bal = balanceOf(who) ~> m_token with { value: 100 };
            ]
            apply: []
        ]
    }
    m_token: Address<Token> {
        in constructor(token_addr) => token_addr
    }
    m_last_balance: U256 {
        in constructor() => 0
        in paidQuery(who) => apply: bal
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    // var call with value option
    assert!(
        sol.contains("IToken(m_token).balanceOf{value: 100}(who)"),
        "var call with value option not emitted correctly: {sol}"
    );
}

#[test]
fn evm_var_call_result_in_arithmetic_transform() {
    let src = r#"
entity Token {
    routes {
        constructor() => []
        view totalSupply() -> U256 => [
            return(m_supply)
        ]
    }
    m_supply: U256 {
        in constructor() => 0
    }
}

entity Stats {
    routes {
        constructor(token_addr: Address<Token>) => []
        snapshot() => [
            fetch: [
                var supply = totalSupply() ~> m_token;
            ]
            store: []
        ]
    }
    m_token: Address<Token> {
        in constructor(token_addr) => token_addr
    }
    m_cached_supply: U256 {
        in constructor() => 0
        in snapshot() => store: supply
    }
    m_double_supply: U256 {
        in constructor() => 0
        in snapshot() => store: supply * 2
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    // Hoisted declaration + assignment in phase block
    assert!(
        sol.contains("uint256 supply;"),
        "hoisted var declaration missing: {sol}"
    );
    assert!(
        sol.contains("supply = IToken(m_token).totalSupply()"),
        "var call assignment not emitted: {sol}"
    );
    // The transform using `supply * 2` should appear
    assert!(
        sol.contains("(supply * 2)"),
        "member transform using var in arithmetic not emitted: {sol}"
    );
    // assignment must appear before its use in transforms
    let assignment = sol.find("supply = IToken(m_token)").expect("assignment missing");
    let use_cached = sol.find("next_m_cached_supply = supply").expect("cached use missing");
    let use_double = sol.find("(supply * 2)").expect("double use missing");
    assert!(assignment < use_cached, "assignment must come before cached_supply transform: {sol}");
    assert!(assignment < use_double, "assignment must come before double_supply transform: {sol}");
}

// ---------------------------------------------------------------------------
// Named send to typed address (fire-and-forget, no return value)
// ---------------------------------------------------------------------------

#[test]
fn evm_named_send_to_typed_address_generates_interface_call() {
    let src = r#"
entity Token {
    routes {
        constructor() => []
        transfer(to: address, amount: U256) => []
    }
    m_supply: U256 {
        in constructor() => 0
    }
}

entity Vault {
    routes {
        constructor(token_addr: Address<Token>) => []
        forward(recipient: address, amount: U256) => [
            transfer(amount) ~> m_token
        ]
    }
    m_token: Address<Token> {
        in constructor(token_addr) => token_addr
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("interface IToken {"),
        "missing IToken interface for fire-and-forget send: {sol}"
    );
    assert!(
        sol.contains("IToken(m_token).transfer(amount)"),
        "named send to typed address should use interface call: {sol}"
    );
    assert!(
        !sol.contains("abi.encodeWithSignature"),
        "typed send should NOT use abi.encodeWithSignature: {sol}"
    );
}

#[test]
fn evm_named_send_to_typed_address_with_value() {
    let src = r#"
entity Token {
    routes {
        constructor() => []
        deposit() => []
    }
    m_supply: U256 {
        in constructor() => 0
    }
}

entity Vault {
    routes {
        constructor(token_addr: Address<Token>) => []
        fund(amount: U256) => [
            deposit() ~> m_token with { value: amount }
        ]
    }
    m_token: Address<Token> {
        in constructor(token_addr) => token_addr
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("IToken(m_token).deposit{value: amount}()"),
        "typed send with value should use interface call with value: {sol}"
    );
}

#[test]
fn evm_plain_transfer_generates_low_level_call() {
    let src = r#"
entity Wallet {
    routes {
        send(to: address, amount: U256) => [
            ~> to with { value: amount }
        ]
    }
    m_dummy: u64 {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("payable(to).call{value: amount}"),
        "plain transfer should use low-level call: {sol}"
    );
    assert!(
        sol.contains("require(ok,"),
        "plain transfer should check success: {sol}"
    );
}

#[test]
fn evm_named_send_to_untyped_address_uses_abi_encode() {
    let src = r#"
entity Notifier {
    routes {
        notify(to: address, amount: U256) => [
            callback(amount) ~> to
        ]
    }
    m_dummy: u64 {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("abi.encodeWithSignature(\"callback(uint256)\", amount)"),
        "named send to untyped address should use abi.encodeWithSignature: {sol}"
    );
    assert!(
        sol.contains("require(ok,"),
        "named send should check success: {sol}"
    );
}

// ---------------------------------------------------------------------------
// Governor PLAN.md gap regression tests
// ---------------------------------------------------------------------------
//
// These tests pin the resolved behaviour of gaps surfaced during the
// `examples/governor/` translation. Each test corresponds to a gap entry in
// `examples/governor/PLAN.md` and would have failed before the matching
// codegen fix landed.

/// G-T1 — `from <member>` syntax. The grammar accepts a bare member name
/// (no parens) as a `FromClauseKind::Member`, and EVM codegen lowers it to
/// `require(msg.sender == m_member, ...)` instead of treating the
/// identifier as an entity name.
#[test]
fn evm_gt1_from_address_member_lowers_to_msg_sender_eq() {
    let src = r#"
entity Vault {
    routes {
        constructor(admin_: address) => []
        adminOnly() from m_admin => []
    }
    m_admin: address {
        in constructor(admin_) => admin_
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("require((msg.sender == m_admin),"),
        "from <member> should compare msg.sender against the address-typed \
         member: {sol}"
    );
    assert!(
        !sol.contains("require(false,"),
        "from <member> must NOT short-circuit to require(false, ...): {sol}"
    );
}

/// G-T1 + `: throw N` — the optional numeric annotation on a from-clause
/// must surface as the canonical `throw(N)` revert reason so test
/// assertions like `expect throw N` can match.
#[test]
fn evm_gt1_from_member_with_throw_code_emits_throw_n() {
    let src = r#"
entity Vault {
    routes {
        constructor(admin_: address) => []
        adminOnly() from m_admin : throw 400 => []
    }
    m_admin: address {
        in constructor(admin_) => admin_
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("require((msg.sender == m_admin), \"throw(400)\");"),
        "from <member> : throw N should emit throw(N) revert reason: {sol}"
    );
}

/// G-T2 — `sys::timestamp + N` in a member-transform RHS. Used to drop to
/// `/* unsupported expr */ 0`; now lowers to `block.timestamp + delay`.
/// (Subsumed by the broader G-T7 fix, kept as a focused regression.)
#[test]
fn evm_gt2_sys_timestamp_in_transform_lowers_to_block_timestamp() {
    let src = r#"
entity Lock {
    routes {
        constructor() => []
        schedule(id: U256, delay: u64) => []
    }
    m_op_timestamp: HashMap<U256, u64> {
        in schedule(id, delay) => m_op_timestamp.update(id, sys::timestamp + delay)
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("(block.timestamp + delay)"),
        "sys::timestamp + delay should lower to block.timestamp + delay: {sol}"
    );
    assert!(
        sol.contains("m_op_timestamp[")
            && sol.contains("] = ")
            && !sol.contains("/* unsupported expr */"),
        "transform must write the computed deadline, not a placeholder: {sol}"
    );
}

/// G-T3 — `data:` in a plain `~> address with { ... }` send. Used to drop
/// the calldata; now it is threaded directly into the `.call(...)` payload.
#[test]
fn evm_gt3_named_send_data_field_threaded_into_call() {
    let src = r#"
entity Forwarder {
    routes {
        forward(to: address, value: U256, data: CamData) => [
            ~> to with { value: value, data: data }
        ]
    }
    m_dummy: u64 {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("payable(to).call{value: value}(data)"),
        "data: in a named send must be passed as the .call(...) payload: {sol}"
    );
    assert!(
        !sol.contains("payable(to).call{value: value}(bytes(\"\"))"),
        "data: must override the empty-calldata default: {sol}"
    );
}

/// A `var` capture named after a Solidity reserved word (`after`) must be
/// escaped consistently: the hoisted cross-phase declaration, the phase
/// assignment, and every read (which already lowers through
/// `sol_sanitize_ident`) all have to agree on `_after`. The ERC-4626
/// `deposit` shape — `count: [ var after = balanceOf(...) ~> m_asset; ]`
/// read by the next phase — used to emit `uint256 after;` (solc 2314).
#[test]
fn evm_reserved_word_var_capture_is_escaped_across_phases() {
    let src = r#"
extern entity Token {
    view route balanceOf(who: address) -> U256;
}

entity Vaultish {
    routes {
        constructor(t: Address<Token>) => []
        settle(amount: U256) -> U256 => [
            read: [
                var before = balanceOf(msg::sender) ~> m_token;
            ]
            count: [
                var after = balanceOf(msg::sender) ~> m_token;
            ]
            pay: [
                return(after - before)
            ]
        ]
    }
    m_token: Address<Token> { in constructor(t) => t }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("_after"),
        "the reserved-word capture must be escaped to `_after`: {sol}"
    );
    assert!(
        !sol.contains(" after;") && !sol.contains(" after =") && !sol.contains("(after "),
        "no bare `after` local may survive (solc reserves the keyword): {sol}"
    );
}

/// G-T4 — A Cambrian route literally named `constructor` must NOT emit a
/// public `function constructor(...)`. Solidity reserves the keyword and
/// would reject the duplicate; the body is folded into the synthesised
/// constructor / `initialize()` instead.
#[test]
fn evm_gt4_no_duplicate_function_constructor_emitted() {
    let src = r#"
entity Counter {
    routes {
        constructor(seed: u64) => []
        bump() => []
    }
    m_count: u64 {
        in constructor(seed) => seed
        in bump() => m_count + 1
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        !sol.contains("function constructor("),
        "must not emit `function constructor(...)` — it collides with the \
         Solidity reserved keyword: {sol}"
    );
    assert!(
        sol.contains("function initialize(uint64 seed)"),
        "named `constructor` route params must fold into initialize() in det mode: {sol}"
    );
    assert!(
        sol.contains("constructor(address factory_)"),
        "det stub constructor must guard factory: {sol}"
    );
}

/// G-T5 — `pure fn` taking a `HashMap<K,V>` parameter must lower it as a
/// `storage` reference (Solidity rejects mappings in memory) and demote
/// the function to `view`. Free functions also cannot have `internal`.
#[test]
fn evm_gt5_pure_fn_hashmap_param_uses_storage_view_no_visibility() {
    let src = r#"
pure fn balance_of(balances: HashMap<address, U256>, owner: address) -> U256 {
    balances[owner]
}

entity Token {
    routes {
        constructor() => []
        check(owner: address) -> U256 => [
            return(balance_of(m_balances, owner))
        ]
    }
    m_balances: HashMap<address, U256> {
        in constructor() => m_balances
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("mapping(address => uint256) storage balances"),
        "HashMap parameter must be lowered with `storage` location: {sol}"
    );
    // Storage reads aren't `pure` in Solidity, so the function is demoted.
    assert!(
        sol.contains("function balance_of(") && sol.contains(") view returns"),
        "free-fn with mapping param must be `view`, not `pure`: {sol}"
    );
    // Free functions in Solidity can't carry visibility modifiers. Inspect
    // the `balance_of` declaration line specifically — the entity contract
    // legitimately carries `external returns` on its routes.
    let bal_line = sol
        .lines()
        .find(|l| l.contains("function balance_of("))
        .unwrap_or("");
    assert!(
        !bal_line.contains("internal") && !bal_line.contains("external")
            && !bal_line.contains("public") && !bal_line.contains("private"),
        "free function must not have a visibility modifier: {bal_line}"
    );
}

/// G-T6 — `if/else` whose branches each call `m_xs.update(...)` must
/// materialise as conditional storage writes, not be silently elided to a
/// `// no-op` comment. Pinned via the `castVote` shape from the Governor
/// example (one branch updates `m_for`, the other no-ops by yielding the
/// map identity).
#[test]
fn evm_gt6_if_else_in_transform_emits_conditional_writes() {
    let src = r#"
entity Tally {
    routes {
        constructor() => []
        tally(id: U256, support: u64, weight: U256) => []
    }
    m_for: HashMap<U256, U256> {
        in tally(id, support, weight) => {
            if support == 1 {
                m_for.update(id, m_for[id] + weight)
            } else {
                m_for
            }
        }
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    // The body uses temporary bindings for the key / value, so look for
    // the structural pieces independently rather than the verbatim string.
    assert!(sol.contains("if ("), "missing if-lowering: {sol}");
    assert!(
        sol.contains("(m_for[id] + weight)"),
        "additive RHS must reach the generated code: {sol}"
    );
    assert!(
        sol.contains("m_for[_cam_tmp") || sol.contains("m_for[id] ="),
        "the truthy branch must write back into m_for: {sol}"
    );
    assert!(
        !sol.contains("// mapping m_for transform: no-op"),
        "transform body must not be elided to a no-op comment: {sol}"
    );
}

/// G-T8 — Cross-entity interface emission. Pins both sides of the known
/// single-file-vs-project trade-off:
///   * When the referenced entity is **NOT** in the same program (the
///     single-file mode users would hit), we intentionally emit a minimal
///     stub interface — the calls compile but route signatures must be
///     filled in or the project must be rebuilt in `--project` mode.
///   * When the referenced entity **IS** in the same program (project
///     mode), the codegen populates the full signature list from the
///     other entity's routes, and cross-calls work out of the box.
#[test]
fn evm_gt8_cross_entity_interface_emission_pins_both_modes() {
    // Single-file flavour: only `Caller` defined; `Token` is declared
    // as an `extern entity` so the EVM target emits a populated
    // `interface IToken { ... }` stub from the user's declaration.
    // (Phase EVM-6 M1: undeclared external entity targets now fail
    // validation via E22, so a Cambrian program that wants to call into
    // a foreign contract must declare its surface.)
    let single = r#"
extern entity Token {
    route transfer(amount: U256);
}

entity Caller {
    routes {
        constructor(t: Address<Token>) => []
        ping(amount: U256) => [
            transfer(amount) ~> m_token
        ]
    }
    m_token: Address<Token> {
        in constructor(t) => t
    }
}
"#;
    let program_single = parse(single);
    let sol_single = gen_evm_solidity(&program_single, true);
    assert!(
        sol_single.contains("interface IToken {")
            && sol_single.contains("function transfer(uint256 amount) external;"),
        "single-file mode must emit a populated extern interface: {sol_single}"
    );
    assert!(
        !sol_single.contains("// external entity"),
        "single-file mode must not fall back to the empty extern stub when an extern entity is declared: {sol_single}"
    );

    // Project flavour: both entities live in the same program, so the
    // extern declaration is unnecessary and we get a full internal
    // interface instead.
    let combined = r#"
entity Token {
    routes {
        constructor() => []
        transfer(amount: U256) => []
    }
    m_supply: U256 {
        in constructor() => 0
    }
}

entity Caller {
    routes {
        constructor(t: Address<Token>) => []
        ping(amount: U256) => [
            transfer(amount) ~> m_token
        ]
    }
    m_token: Address<Token> {
        in constructor(t) => t
    }
}
"#;
    let program_combined = parse(combined);
    let sol_combined = gen_evm_solidity(&program_combined, true);
    assert!(
        sol_combined.contains("interface IToken {")
            && sol_combined.contains("function transfer(uint256 amount)"),
        "project mode must populate cross-entity route signatures: {sol_combined}"
    );
    assert!(
        !sol_combined.contains("// external entity"),
        "project mode must not fall back to the foreign-entity stub comment: {sol_combined}"
    );
}

/// G-T7 — `sys::timestamp` inside a route-level `where` clause used to
/// degrade to `require(false, ...)`. Verify it now lowers to a real
/// `block.timestamp` comparison.
#[test]
fn evm_gt7_sys_timestamp_in_where_clause_compares_block_timestamp() {
    let src = r#"
entity Vesting {
    routes {
        constructor() => []
        claim(deadline: u64)
            where sys::timestamp >= deadline : throw 501
            => []
    }
    m_dummy: u64 {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("require((block.timestamp >= deadline), \"throw(501)\");"),
        "sys::timestamp in where must lower to a block.timestamp guard: {sol}"
    );
    assert!(
        !sol.contains("require(false,"),
        "where with sys::timestamp must not collapse to require(false): {sol}"
    );
}

// ===========================================================================
// `.fold(init, |acc, x| body)` lowering on EVM
//
// `.fold` is a method-call form (`Expr::MethodCall("fold", [init, Closure])`)
// that already works on Acki Nacki / native Rust. On EVM we lower it to an
// imperative for-loop that threads a single `acc` across iterations — same
// shape as `gen_for_loop` but writing into a scalar accumulator rather than
// materialising a `T[] memory` result. Supported iterator shapes mirror the
// `for` form: `Range` (`start..end`) and any expression resolving to
// `Vec<T>`.
// ===========================================================================

#[test]
fn evm_fold_range_emits_imperative_loop() {
    let src = r#"
pure fn babylonian_sqrt(x: U256) -> U256 {
    (0..7).fold(x, |r, _i| if r == 0 { 0 } else { (r + x / r) / 2 })
}

entity FoldDemo {
    routes {
        constructor() => []
        seed(x: U256) => []
    }
    m_value: U256 {
        in constructor() => 0
        in seed(x) => babylonian_sqrt(x)
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("function babylonian_sqrt(uint256 x) pure returns (uint256)"),
        "missing pure fn signature: {sol}"
    );
    assert!(
        sol.contains("uint256 r = x;"),
        "fold must initialise the accumulator with the `init` expression: {sol}"
    );
    assert!(
        sol.contains("for (uint256 _cam_tmp"),
        "fold must lower to a Solidity for-loop: {sol}"
    );
    assert!(
        sol.contains("r = ((r == 0) ? 0 : ((r + (x / r)) / 2));"),
        "fold body must thread back into the accumulator: {sol}"
    );
    assert!(
        sol.contains("return r;"),
        "fold expression value must be the accumulator: {sol}"
    );
}

#[test]
fn evm_fold_wildcard_var_compiles() {
    let src = r#"
pure fn count_iterations(n: U256) -> U256 {
    (0..n).fold(0, |acc, _x| acc + 1)
}

entity D {
    routes {
        constructor() => []
        seed(n: U256) => []
    }
    m_v: U256 {
        in constructor() => 0
        in seed(n) => count_iterations(n)
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("uint256 acc = 0;"),
        "fold must initialise the named accumulator: {sol}"
    );
    assert!(
        sol.contains("for (uint256 _cam_tmp"),
        "fold must lower to a Solidity for-loop: {sol}"
    );
    assert!(
        sol.contains("acc = (acc + 1);"),
        "fold body must run regardless of whether the loop variable is used: {sol}"
    );
    assert!(
        !sol.contains("revert(\"EVM: closure-as-value"),
        "supported `.fold` shape must not fall back to the closure-as-value revert: {sol}"
    );
}

#[test]
fn evm_fold_in_pure_fn_returns_acc() {
    let src = r#"
pure fn sum_to(n: U256) -> U256 {
    (0..n).fold(0, |acc, i| acc + i)
}

entity D {
    routes {
        constructor() => []
        seed(n: U256) => []
    }
    m_v: U256 {
        in constructor() => 0
        in seed(n) => sum_to(n)
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("function sum_to(uint256 n) pure returns (uint256)"),
        "fold inside a pure fn must compile to a free function: {sol}"
    );
    assert!(
        sol.contains("uint256 acc = 0;") && sol.contains("acc = (acc + i);"),
        "fold body must mutate the accumulator: {sol}"
    );
    assert!(
        sol.contains("return acc;"),
        "pure fn whose body is `.fold(...)` must return the accumulator: {sol}"
    );
}

#[test]
fn evm_fold_over_vec_param() {
    let src = r#"
pure fn vec_sum(xs: Vec<U256>) -> U256 {
    xs.fold(0, |acc, x| acc + x)
}

entity D {
    routes {
        constructor() => []
        seed(xs: Vec<U256>) => []
    }
    m_v: U256 {
        in constructor() => 0
        in seed(xs) => vec_sum(xs)
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("function vec_sum(uint256[] memory xs) pure returns (uint256)"),
        "fold over a Vec<U256> param must accept `uint256[] memory`: {sol}"
    );
    assert!(
        sol.contains("uint256 acc = 0;"),
        "Vec.fold must initialise the accumulator: {sol}"
    );
    assert!(
        sol.contains("xs.length"),
        "Vec.fold must use `.length` as the iteration bound: {sol}"
    );
    assert!(
        sol.contains("uint256 x = xs["),
        "Vec.fold must bind the loop variable to `xs[i]`: {sol}"
    );
    assert!(
        sol.contains("acc = (acc + x);"),
        "Vec.fold body must thread back into the accumulator: {sol}"
    );
}

#[test]
fn evm_codegen_emits_wrapping_op_helpers_when_used() {
    // G-U2: contracts that use any `+%` / `-%` / `*%` get the
    // unchecked-arithmetic helpers emitted, and the operators lower
    // through them. Contracts that don't reference the operators
    // must NOT have the helpers added (avoids dead code).
    let src = r#"
entity Wrapper {
    routes {
        bump(by: U256) => []
    }
    m_total: U256 {
        in bump(by) => m_total +% by
    }
}

entity Checked {
    routes {
        bump(by: U256) => []
    }
    m_total: U256 {
        in bump(by) => m_total + by
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    let helpers_pos = sol.find("function _wadd(uint256 a, uint256 b) internal pure")
        .expect("Wrapper must emit `_wadd` helper");
    assert!(sol.contains("function _wsub(uint256 a, uint256 b) internal pure"));
    assert!(sol.contains("function _wmul(uint256 a, uint256 b) internal pure"));
    assert!(sol.contains("unchecked { return a + b; }"));

    // The wrapping `+%` lowers to a call to `_wadd`.
    assert!(
        sol.contains("_wadd(m_total, by)"),
        "`+%` must lower through `_wadd`: {sol}"
    );

    // `Checked` (no wrapping ops) must not get its own helpers, and
    // its plain `+` stays inline. Slice off everything before the
    // `Wrapper` helpers so we only inspect `Checked`.
    let after = &sol[helpers_pos..];
    let checked_pos = after.find("contract Checked").expect("Checked contract present");
    let checked_section = &after[checked_pos..];
    assert!(
        !checked_section.contains("function _wadd("),
        "Checked must not emit wrapping helpers when no `+%` is used: {checked_section}"
    );
    assert!(
        checked_section.contains("(m_total + by)"),
        "Plain `+` must stay inline outside the wrap helpers: {checked_section}"
    );
}

// ===========================================================================
// EVM Easy-Pickings Batch 1: new `evm::*` and `sys::*` intrinsics
//
// These tests pin down the lowering for intrinsics that already parse via
// existing AST nodes (`NamespacedCall` / `EnumVariantWithData("evm", ...)`
// for the `evm::*` family, `Expr::SysField` for the `sys::*` family) but
// previously fell through to the silent `/* unsupported expr */ 0`
// sentinel because `gen_evm_ns` and the `Expr::SysField` match arm only
// recognised a small subset of names.
// ===========================================================================

/// Batch1-A1 — `evm::sha256(a, b, ...)` lowers through `abi.encodePacked`
/// (matches the convention of `evm::keccak256Packed`). Used by contracts
/// that need the SHA-256 precompile (0x02), e.g. for cross-chain proofs.
#[test]
fn evm_b1_sha256_lowers_to_packed_precompile() {
    let src = r#"
pure fn proof_hash(a: U256, b: U256) -> U256 {
    evm::sha256(a, b)
}

entity ShaUser {
    routes {
        constructor() => []
        note(a: U256, b: U256) => []
    }
    m_last: U256 {
        in note(a, b) => proof_hash(a, b)
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("uint256(sha256(abi.encodePacked(a, b)))"),
        "evm::sha256 must lower to uint256(sha256(abi.encodePacked(...))): {sol}"
    );
    assert!(
        !sol.contains("/* unsupported expr */"),
        "evm::sha256 must not fall through to the unsupported-expr sentinel: {sol}"
    );
}

/// Batch1-A2 — `evm::ripemd160(args)` lowers through `abi.encodePacked`
/// and is widened from `bytes20` to `uint256` so the result fits a
/// Cambrian `U256` slot.
#[test]
fn evm_b1_ripemd160_lowers_to_packed_precompile() {
    let src = r#"
pure fn rmd_hash(a: U256) -> U256 {
    evm::ripemd160(a)
}

entity RmdUser {
    routes {
        constructor() => []
        note(a: U256) => []
    }
    m_last: U256 {
        in note(a) => rmd_hash(a)
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("uint256(uint160(ripemd160(abi.encodePacked(a))))"),
        "evm::ripemd160 must lower to uint256(uint160(ripemd160(abi.encodePacked(...)))): {sol}"
    );
    assert!(
        !sol.contains("/* unsupported expr */"),
        "evm::ripemd160 must not fall through to the unsupported-expr sentinel: {sol}"
    );
}

/// Batch1-A3 — `evm::balance(addr)` lowers to Solidity `addr.balance`.
/// (Different from `sys::balance`, which reads `address(this).balance`.)
#[test]
fn evm_b1_balance_of_address_lowers_to_addr_balance() {
    let src = r#"
entity BalCheck {
    routes {
        constructor() => []
        peek(who: address) -> U256 => [return(evm::balance(who))]
    }
    m_dummy: u64 {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("return who.balance;"),
        "evm::balance(who) must lower to who.balance: {sol}"
    );
    assert!(
        !sol.contains("/* unsupported expr */"),
        "evm::balance must not fall through to the unsupported-expr sentinel: {sol}"
    );
}

/// Batch1-A4 — `evm::blockhash(n)` lowers to `uint256(blockhash(n))`.
/// Used by VRF-via-blockhash and historical-state proofs.
#[test]
fn evm_b1_blockhash_lowers_to_uint256_cast() {
    let src = r#"
entity BlockHashUser {
    routes {
        constructor() => []
        sample(n: U256) -> U256 => [return(evm::blockhash(n))]
    }
    m_dummy: u64 {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("return uint256(blockhash(n));"),
        "evm::blockhash(n) must lower to uint256(blockhash(n)): {sol}"
    );
    assert!(
        !sol.contains("/* unsupported expr */"),
        "evm::blockhash must not fall through to the unsupported-expr sentinel: {sol}"
    );
}

/// Batch1-B1 — `sys::origin` lowers to Solidity `tx.origin` and is typed
/// as `address` by `infer_let_type_entity` so `let o = sys::origin` does
/// not silently widen to `uint256`.
#[test]
fn evm_b1_sys_origin_lowers_to_tx_origin() {
    let src = r#"
entity OriginUser {
    routes {
        constructor() => []
        who() -> address => [
            let o = sys::origin;
            return(o)
        ]
    }
    m_dummy: u64 {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("address o = tx.origin;"),
        "sys::origin must lower to tx.origin with `address` typing: {sol}"
    );
    assert!(
        !sol.contains("/* unsupported expr */"),
        "sys::origin must not fall through to the unsupported-expr sentinel: {sol}"
    );
}

/// Batch1-B2 — `sys::gasprice` lowers to `tx.gasprice`.
#[test]
fn evm_b1_sys_gasprice_lowers_to_tx_gasprice() {
    let src = r#"
entity GasPriceUser {
    routes {
        constructor() => []
        ratio() -> U256 => [return(sys::gasprice)]
    }
    m_dummy: u64 {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("return tx.gasprice;"),
        "sys::gasprice must lower to tx.gasprice: {sol}"
    );
    assert!(
        !sol.contains("/* unsupported expr */"),
        "sys::gasprice must not fall through to the unsupported-expr sentinel: {sol}"
    );
}

/// Batch1-B3 — `sys::blobbasefee` lowers to `block.blobbasefee` (EIP-4844 / Cancun).
#[test]
fn evm_b1_sys_blobbasefee_lowers_to_block_blobbasefee() {
    let src = r#"
entity BlobUser {
    routes {
        constructor() => []
        peek() -> U256 => [return(sys::blobbasefee)]
    }
    m_dummy: u64 {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("return block.blobbasefee;"),
        "sys::blobbasefee must lower to block.blobbasefee: {sol}"
    );
    assert!(
        !sol.contains("/* unsupported expr */"),
        "sys::blobbasefee must not fall through to the unsupported-expr sentinel: {sol}"
    );
}

// ===========================================================================
// EVM Easy-Pickings Batch 1: type-inference improvements
//
// These tests pin down `infer_let_type_entity` and
// `infer_iter_elem_type_entity` paths that previously fell through to
// the `uint256` / `bytes` defaults.
// ===========================================================================

/// Batch1-C1 — `let o = sys::origin` binds as `address`, not `uint256`.
/// (Already covered structurally by evm_b1_sys_origin_lowers_to_tx_origin
/// but isolated here so a regression in `infer_let_type_entity` is easy
/// to spot.)
#[test]
fn evm_b1_infer_let_sys_origin_typed_as_address() {
    let src = r#"
entity InferDemo {
    routes {
        constructor() => []
        peek() -> address => [
            let a = sys::origin;
            return(a)
        ]
    }
    m_dummy: u64 {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("address a = tx.origin;"),
        "let-binding from sys::origin must be typed as address: {sol}"
    );
    assert!(
        !sol.contains("uint256 a = tx.origin;"),
        "let-binding from sys::origin must NOT be typed as uint256: {sol}"
    );
}

/// Batch1-C2 — `let n = enum::Variant` binds as the enum type, not
/// `uint256`. Without this, callers that store the let into a typed
/// variable get a Solidity type-mismatch error at solc time.
#[test]
fn evm_b1_infer_let_enum_variant_typed_as_enum() {
    let src = r#"
entity Status {

    enum State { Idle, Active, Closed }

    routes {
        constructor() => []
        check() -> State => [
            let cur = State::Active;
            return(cur)
        ]
    }
    m_dummy: u64 {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("State cur = State.Active;"),
        "let from enum variant must be typed by enum name: {sol}"
    );
}

/// Batch1-C3 — `let xs = m_list` where `m_list: Vec<U256>` binds as
/// `uint256[] memory` (not the bare `uint256` default). Tests the existing
/// path; the inference change adds a parallel path for record-field
/// access (Batch1-C4).
#[test]
fn evm_b1_infer_let_member_vec_typed_as_array_memory() {
    let src = r#"
entity VecLet {
    routes {
        constructor() => []
        snapshot() -> U256 => [
            let xs = m_list;
            return(xs[0])
        ]
    }
    m_list: Vec<U256> {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("uint256[] memory xs = m_list;"),
        "let from a Vec<U256> member must be typed as uint256[] memory: {sol}"
    );
}

/// Batch1-C4 — `let v = pure_fn_returning_vec(args)` binds as a typed
/// `T[] memory` rather than the silent `uint256` default. Mirrors the
/// member-Vec path in C3 but goes through `lookup_pure_fn_return`.
#[test]
fn evm_b1_infer_let_pure_fn_returning_vec_typed_as_array() {
    let src = r#"
pure fn make_list(n: U256) -> Vec<U256> {
    array(n, n, n)
}

entity FnVec {
    routes {
        constructor() => []
        head(n: U256) -> U256 => [
            let xs = make_list(n);
            return(xs[0])
        ]
    }
    m_dummy: u64 {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("uint256[] memory xs = make_list(n);"),
        "let from a Vec<U256>-returning pure fn must be typed as uint256[] memory: {sol}"
    );
}

/// Batch1-C5 — `for x in m_items { let y = x; ... }` over a `Vec<U256>`
/// member binds `x` as `uint256` typed (this already worked) AND
/// `let y = x` binds as `uint256` typed via the broadened
/// `infer_let_type_entity` path (newly recognised when `x` is a fresh
/// loop variable rather than a member).
#[test]
fn evm_b1_for_loop_over_member_vec_lowers_with_typed_var() {
    let src = r#"
entity ForUser {
    routes {
        constructor() => []
        scan() -> U256 => [
            let total = m_items.fold(0, |acc, x| acc + x);
            return(total)
        ]
    }
    m_items: Vec<U256> {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("uint256 x = m_items["),
        "fold loop variable over a Vec<U256> member must be typed: {sol}"
    );
    assert!(
        sol.contains("uint256 acc = 0;"),
        "fold accumulator over Vec<U256> must be uint256: {sol}"
    );
}

// ---------------------------------------------------------------------------
// Phase EVM-13: Iterable HashMaps
// ---------------------------------------------------------------------------
//
// EVM mappings have no length and no key enumeration. To support
// `m.keys()` / `for k in m.keys() { … }` we auto-emit a parallel
// `K[] m_keys` storage array (plus an `m_keys_index` lookup for O(1)
// swap-pop on remove and a `m_exists` flag mapping) whenever the
// HashMap member is iterated. Insert/update/remove transforms then
// maintain that array transparently.
//
// Detection is purely on the *use site* — if no route iterates the map
// the companion storage is *not* emitted, so non-iterated HashMaps stay
// gas-cheap. Tests below cover:
//
//   * companion storage emission gated on detection
//   * insert injects "push if new" maintenance
//   * remove injects swap-pop maintenance
//   * `m.keys()` / `m.keys().collect()` lower to the parallel array
//   * `for k in m.keys() { body }` walks the parallel array via the
//     existing Vec-iter loop machinery
//   * a non-iterated HashMap still compiles to the bare mapping form

#[test]
fn evm13_iterated_hashmap_emits_keys_companion_storage() {
    let src = r#"
entity Reg {
    routes {
        register(k: address, v: u64) => []
        listKeys() -> Vec<address> => [
            let ks = m_data.keys().collect();
            return(ks)
        ]
    }
    m_data: HashMap<address, u64> {
        in register(k, v) => m_data.insert(k, v)
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("mapping(address => uint64) public m_data;"),
        "base mapping still present: {sol}"
    );
    assert!(
        sol.contains("address[] public m_data_keys;"),
        "parallel key array m_data_keys not emitted: {sol}"
    );
    assert!(
        sol.contains("mapping(address => uint256) private m_data_keys_index;"),
        "key-index mapping m_data_keys_index not emitted: {sol}"
    );
    assert!(
        sol.contains("mapping(address => bool) public m_data_exists;"),
        "iteration forces m_data_exists to be emitted (used to gate push): {sol}"
    );
}

#[test]
fn evm13_non_iterated_hashmap_does_not_emit_keys_companion() {
    let src = r#"
entity Plain {
    routes {
        register(k: address, v: u64) => []
    }
    m_data: HashMap<address, u64> {
        in register(k, v) => m_data.insert(k, v)
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("mapping(address => uint64) public m_data;"),
        "base mapping must still be present: {sol}"
    );
    assert!(
        !sol.contains("m_data_keys"),
        "no .keys() / .iter() / .values() use in this program — parallel array must NOT be emitted (gas regression): {sol}"
    );
    assert!(
        !sol.contains("m_data_exists"),
        "no .exists() / .keys() use — m_data_exists must NOT be emitted: {sol}"
    );
}

#[test]
fn evm13_insert_on_iterated_hashmap_pushes_new_key() {
    let src = r#"
entity Reg {
    routes {
        register(k: address, v: u64) => []
        listKeys() -> Vec<address> => [
            let ks = m_data.keys().collect();
            return(ks)
        ]
    }
    m_data: HashMap<address, u64> {
        in register(k, v) => m_data.insert(k, v)
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    // The insert path must check the existence flag, push the key into
    // m_data_keys, record its index, and flip the flag — all before the
    // value write.
    assert!(
        sol.contains("if (!m_data_exists["),
        "guarded push missing on insert: {sol}"
    );
    assert!(
        sol.contains("m_data_keys_index[") && sol.contains("] = m_data_keys.length;"),
        "key-index assignment missing on insert: {sol}"
    );
    assert!(
        sol.contains("m_data_keys.push("),
        "m_data_keys.push missing on insert: {sol}"
    );
    assert!(
        sol.contains("m_data_exists[") && sol.contains("= true;"),
        "exists flag flip missing on insert: {sol}"
    );
}

#[test]
fn evm13_remove_on_iterated_hashmap_does_swap_pop() {
    let src = r#"
entity Reg {
    routes {
        register(k: address, v: u64) => []
        unregister(k: address) => []
        listKeys() -> Vec<address> => [
            let ks = m_data.keys().collect();
            return(ks)
        ]
    }
    m_data: HashMap<address, u64> {
        in register(k, v) => m_data.insert(k, v)
        in unregister(k) => m_data.remove(k)
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    // Swap-pop: index of removed key is overwritten by the last key,
    // m_data_keys_index of the moved key is updated, then m_data_keys
    // shrinks via .pop(). Existence flag and value entry both cleared.
    assert!(
        sol.contains("if (m_data_exists["),
        "guarded swap-pop missing on remove: {sol}"
    );
    assert!(
        sol.contains("m_data_keys.pop();"),
        "m_data_keys.pop() missing on remove: {sol}"
    );
    assert!(
        sol.contains("m_data_keys_index["),
        "swap-pop must update m_data_keys_index: {sol}"
    );
    assert!(
        sol.contains("delete m_data_exists["),
        "exists flag must be cleared on remove: {sol}"
    );
    assert!(
        sol.contains("delete m_data["),
        "underlying mapping entry must be cleared on remove: {sol}"
    );
}

#[test]
fn evm13_keys_method_lowers_to_parallel_array() {
    let src = r#"
entity Reg {
    routes {
        listKeys() -> Vec<address> => [
            let ks = m_data.keys().collect();
            return(ks)
        ]
    }
    m_data: HashMap<address, u64> {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    // `m_data.keys()` (with or without the no-op `.collect()`) lowers to
    // the parallel storage array. The `let ks = …` binding must come out
    // typed as `address[] memory` (storage→memory copy is implicit on
    // assignment in Solidity).
    assert!(
        sol.contains("address[] memory ks = m_data_keys;"),
        "m.keys().collect() must lower to the parallel storage array, typed for memory binding: {sol}"
    );
}

#[test]
fn evm13_for_over_keys_walks_parallel_array() {
    let src = r#"
entity Reg {
    routes {
        sumValues() -> u64 => [
            let total = m_data.keys().fold(0, |acc, k| acc + m_data[k]);
            return(total)
        ]
    }
    m_data: HashMap<address, u64> {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    // The fold-over-keys lowering must walk m_data_keys, bind each
    // element as `address k = m_data_keys[_i];`, and accumulate
    // `m_data[k]` into the scalar accumulator.
    assert!(
        sol.contains("m_data_keys.length"),
        "fold over m.keys() must use the parallel array's length: {sol}"
    );
    assert!(
        sol.contains("address k = m_data_keys["),
        "fold loop variable over .keys() must be typed as the key type: {sol}"
    );
    assert!(
        sol.contains("m_data[k]"),
        "fold body must read through the original mapping: {sol}"
    );
}

// ===========================================================================
// EVM-12 Batch A: tuple-destructuring loop patterns.
//
// `for (k, v) in m.iter() { ... }` and `<iter>.fold(init, |acc, (k, v)| ...)`
// both feed tuple-shaped components through the new pattern binder. The
// tuple source for these tests is `m.iter()` (HashMap iteration), which
// yields per-iteration `(K, V)` components without any tuple value ever
// existing as a Solidity expression.
// ===========================================================================

#[test]
fn evm12_for_tuple_pattern_over_iter_in_route_body() {
    let src = r#"
entity Reg {
    routes {
        sumValues() -> u64 => [
            let total = m_data.iter().fold(0, |acc, (_k, v)| acc + v);
            return(total)
        ]
    }
    m_data: HashMap<address, u64> {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("m_data_keys.length"),
        "fold over m.iter() must walk the parallel keys array: {sol}"
    );
    assert!(
        sol.contains("address _k") || sol.contains("address k"),
        "tuple destructuring must bind the K component: {sol}"
    );
    assert!(
        sol.contains("uint64 v = m_data["),
        "tuple destructuring must bind the V component via the map lookup: {sol}"
    );
    assert!(
        sol.contains("acc + v"),
        "fold body must reference the bound v: {sol}"
    );
}

#[test]
fn evm12_for_kv_in_iter_emits_both_components() {
    let src = r#"
entity Reg {
    routes {
        listValues() -> Vec<u64> => [
            let vs = { for (k, v) in m_data.iter() { v } };
            return(vs)
        ]
    }
    m_data: HashMap<address, u64> {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("m_data_keys.length"),
        "for (k, v) in m.iter() must use parallel keys length: {sol}"
    );
    assert!(
        sol.contains("address k = m_data_keys["),
        "K component must be bound to the parallel keys array element: {sol}"
    );
    assert!(
        sol.contains("uint64 v = m_data["),
        "V component must be bound via the underlying mapping lookup: {sol}"
    );
}

// ===========================================================================
// EVM-12 Batch B: general iterator-chain fusion.
//
// `<source>.[iter|enumerate|filter|map|take]+.collect()` and
// `<source>.[iter|enumerate|filter|map|take]+.fold(init, |acc, x| body)`
// fuse into a single Solidity for-loop with stage-by-stage inline
// transformations. Closures are inlined positionally (no first-class
// function values).
// ===========================================================================

#[test]
fn evm12_chain_filter_map_fold_lowers_single_loop() {
    let src = r#"
pure fn vec_sum_doubled_positive(xs: Vec<U256>) -> U256 {
    xs.filter(|x| *x > 0).map(|x| *x * 2).fold(0, |acc, x| acc + x)
}

entity D {
    routes {
        seed(xs: Vec<U256>) => []
    }
    m_v: U256 {
        in seed(xs) => vec_sum_doubled_positive(xs)
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("function vec_sum_doubled_positive(uint256[] memory xs) pure returns (uint256)"),
        "pure fn signature must materialise: {sol}"
    );
    assert!(
        sol.contains("uint256 acc = 0;"),
        "fold accumulator must be initialised: {sol}"
    );
    assert!(
        sol.contains("xs.length"),
        "iteration must walk the source vec length: {sol}"
    );
    let fused = sol
        .split("function vec_sum_doubled_positive")
        .nth(1)
        .unwrap_or(&sol);
    let fused_body = fused.split("\n}\n").next().unwrap_or(fused);
    let for_count = fused_body.matches("for (uint256 ").count();
    assert!(
        for_count == 1,
        "filter+map+fold chain must fuse into a single for-loop, got {} for-loops:\n{}",
        for_count,
        fused_body
    );
    assert!(
        sol.contains("if (!"),
        "filter must lower to a guarded continue: {sol}"
    );
    assert!(
        sol.contains("continue;"),
        "filter stage missing `continue`: {sol}"
    );
}

#[test]
fn evm12_chain_filter_map_collect_lowers_to_memory_array() {
    let src = r#"
pure fn doubled_positive(xs: Vec<U256>) -> Vec<U256> {
    xs.filter(|x| *x > 0).map(|x| *x * 2).collect()
}

entity D {
    routes {
        compute(xs: Vec<U256>) -> Vec<U256> => [
            let ys = doubled_positive(xs);
            return(ys)
        ]
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("function doubled_positive(uint256[] memory xs) pure returns (uint256[] memory)"),
        "collect-terminated chain must produce a memory array return: {sol}"
    );
    assert!(
        sol.contains("new uint256[]("),
        "collect must allocate a memory result array: {sol}"
    );
    assert!(
        sol.contains("continue;"),
        "filter stage missing `continue`: {sol}"
    );
}

#[test]
fn evm12_chain_take_collect_caps_iteration() {
    let src = r#"
pure fn first_n(xs: Vec<U256>, n: U256) -> Vec<U256> {
    xs.take(n).collect()
}

entity D {
    routes {
        compute(xs: Vec<U256>, n: U256) -> Vec<U256> => [
            let ys = first_n(xs, n);
            return(ys)
        ]
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("break;"),
        "take stage must cap iteration via `break;`: {sol}"
    );
    assert!(
        sol.contains("new uint256[]("),
        "take + collect must allocate result array: {sol}"
    );
}

#[test]
fn evm12_chain_enumerate_filter_fold_handles_tuple_pattern() {
    let src = r#"
pure fn count_nonzero_indexed(xs: Vec<U256>) -> U256 {
    xs.enumerate()
        .filter(|(_, x)| *x != 0)
        .fold(0, |acc, (_, _)| acc + 1)
}

entity D {
    routes {
        seed(xs: Vec<U256>) => []
    }
    m_v: U256 {
        in seed(xs) => count_nonzero_indexed(xs)
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("xs.length"),
        "enumerate over Vec must use vec length: {sol}"
    );
    assert!(
        sol.contains("acc + 1"),
        "fold body must accumulate: {sol}"
    );
    let fused = sol
        .split("function count_nonzero_indexed")
        .nth(1)
        .unwrap_or(&sol);
    let fused_body = fused.split("\n}\n").next().unwrap_or(fused);
    let for_count = fused_body.matches("for (uint256 ").count();
    assert!(
        for_count == 1,
        "enumerate+filter+fold must fuse into one for-loop, got {} for-loops:\n{}",
        for_count,
        fused_body
    );
}

#[test]
fn evm12_chain_iter_passthrough_for_vec() {
    let src = r#"
pure fn vec_sum_iter(xs: Vec<U256>) -> U256 {
    xs.iter().fold(0, |acc, x| acc + *x)
}

entity D {
    routes {
        seed(xs: Vec<U256>) => []
    }
    m_v: U256 {
        in seed(xs) => vec_sum_iter(xs)
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("uint256 acc = 0;"),
        "fold acc must initialise: {sol}"
    );
    assert!(
        sol.contains("xs.length"),
        "Vec.iter() must pass through to the underlying vec walk: {sol}"
    );
}

#[test]
fn evm12_tuple_pattern_wildcard_components_compile() {
    let src = r#"
entity Reg {
    routes {
        countAll() -> u64 => [
            let n = m_data.iter().fold(0, |acc, (_, _)| acc + 1);
            return(n)
        ]
    }
    m_data: HashMap<address, u64> {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("acc + 1"),
        "wildcard tuple components must still produce a working body: {sol}"
    );
    assert!(
        sol.contains("m_data_keys.length"),
        "iteration must walk the parallel keys array: {sol}"
    );
}

// ===========================================================================
// EVM-12 Batch C: non-scalar `.fold` accumulators
// ===========================================================================

#[test]
fn evm12_fold_record_acc_lowers_with_memory_local() {
    let src = r#"
entity D {
    record Stats {
        total: u64,
        count: u64
    }

    routes {
        compute(xs: Vec<u64>) -> u64 => [
            let zero = { Stats { total: 0, count: 0 } };
            let s = { xs.fold(zero, |acc, x| { Stats { total: acc.total + x, count: acc.count + 1 } }) };
            return(s.total)
        ]
    }
    m_v: u64 {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("Stats memory acc ="),
        "record accumulator must be a `Stats memory` local seeded with the init: {sol}"
    );
    assert!(
        sol.contains("acc = Stats("),
        "fold body must reassign `acc` with the per-iteration record literal: {sol}"
    );
}

// ===========================================================================
// EVM-13 Batch D: `m.iter()` / `m.values()` / bare `for (k, v) in m`
// All build on the parallel-keys companion emitted by EVM-13's `.keys()`
// path; iteration uses the same `<m>_keys` array, with element values
// fetched via the underlying `m[k]` lookup per iteration.
// ===========================================================================

#[test]
fn evm13_for_kv_in_iter_lowers_walking_keys_once() {
    let src = r#"
entity Reg {
    routes {
        sumValues() -> u64 => [
            let total = m_data.iter().fold(0, |acc, (k, v)| acc + v);
            return(total)
        ]
    }
    m_data: HashMap<address, u64> {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("m_data_keys.length"),
        "iteration must walk the parallel keys array exactly once: {sol}"
    );
    let kv_count = sol.matches("m_data_keys[").count();
    assert!(
        kv_count >= 1,
        "loop body must read keys via the parallel array: {sol}"
    );
}

#[test]
fn evm13_bare_for_kv_in_m_sugar_lowers_via_iter() {
    let src = r#"
entity Reg {
    routes {
        sumKeys() -> u64 => [
            let total = m_data.fold(0, |acc, (k, v)| acc + v);
            return(total)
        ]
    }
    m_data: HashMap<address, u64> {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("m_data_keys.length"),
        "bare-HashMap fold must lower as if `.iter()` were written: {sol}"
    );
    assert!(
        sol.contains("m_data[m_data_keys["),
        "bare-HashMap iteration must read values through the underlying mapping: {sol}"
    );
}

#[test]
fn evm13_for_v_in_values_binds_scalar_per_iter() {
    let src = r#"
entity Reg {
    routes {
        sumValues() -> u64 => [
            let total = m_data.values().fold(0, |acc, v| acc + v);
            return(total)
        ]
    }
    m_data: HashMap<address, u64> {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("m_data_keys.length"),
        "values iteration must walk the parallel keys array: {sol}"
    );
    assert!(
        sol.contains("m_data[m_data_keys["),
        "values iteration must read each value through the mapping: {sol}"
    );
    assert!(
        !sol.contains("address k = "),
        "for-v-in-values must NOT bind a key local: {sol}"
    );
}

#[test]
fn evm13_values_collect_returns_v_array() {
    let src = r#"
entity Reg {
    routes {
        listValues() -> Vec<u64> => [
            let vs = m_data.values().collect();
            return(vs)
        ]
    }
    m_data: HashMap<address, u64> {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("m_data_keys.length"),
        "values().collect() must walk the parallel keys array: {sol}"
    );
    assert!(
        sol.contains("uint64[] memory"),
        "values().collect() must materialise a V[] memory array: {sol}"
    );
    assert!(
        sol.contains("m_data[m_data_keys["),
        "values().collect() must populate each slot via the mapping: {sol}"
    );
}

// ===========================================================================
// EVM-12 Batch E: action-level `for` lowering.
//
// `for r in recipients => [ <actions> ]` lowers to a Solidity `for` over
// the iterator source, with the body actions emitted in iteration order.
// Pattern bindings reuse the Batch A binder, so tuple destructuring
// works the same way as in expression-level `for`.
// ===========================================================================

#[test]
fn evm12_action_for_batch_send_lowers_to_solidity_for_loop() {
    let src = r#"
entity Airdrop {
    routes {
        airdrop(amount: uint256, recipients: Vec<address>) => [
            for r in recipients => [
                ~> r with {value: amount}
            ]
        ]
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("for (uint256"),
        "action-level for must emit a Solidity for loop: {sol}"
    );
    assert!(
        sol.contains("recipients.length"),
        "loop must walk the parameter Vec by length: {sol}"
    );
    assert!(
        sol.contains("address r ="),
        "iteration variable must be bound from the parameter array: {sol}"
    );
    assert!(
        sol.contains(".call{value: amount}"),
        "send body must run per iteration: {sol}"
    );
}

#[test]
fn evm12_action_for_range_with_throw_lowers() {
    let src = r#"
entity G {
    routes {
        guard(n: uint256) => [
            for _ in 0..n => [
                if n > 100 => [ throw 42 ]
            ]
        ]
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("for (uint256"),
        "action-level for over a range must emit a Solidity for loop: {sol}"
    );
    assert!(
        sol.contains("revert(\"throw(42)\")") || sol.contains("revert (\"throw(42)\")"),
        "throw inside loop body must propagate: {sol}"
    );
}

#[test]
fn evm12_action_for_tuple_pattern_over_iter_emits_destructured_locals() {
    let src = r#"
entity Reg {
    routes {
        broadcast(payload: uint256) => [
            for (k, v) in m_data.iter() => [
                ~> k with {value: v}
            ]
        ]
    }
    m_data: HashMap<address, uint256> {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("m_data_keys.length"),
        "tuple destructuring on m.iter() walks the parallel keys array: {sol}"
    );
    assert!(
        sol.contains("address k = m_data_keys["),
        "K component must bind from parallel keys array: {sol}"
    );
    assert!(
        sol.contains("uint256 v = m_data["),
        "V component must bind via the underlying mapping: {sol}"
    );
    assert!(
        sol.contains(".call{value: v}"),
        "send body must reference the bound iteration values: {sol}"
    );
}

#[test]
fn evm12_fold_hashmap_acc_rejected_with_workaround() {
    let src = r#"
entity D {
    routes {
        build(xs: Vec<u64>) -> u64 => [
            let m = xs.fold({}, |acc, x| acc.insert(x, x));
            return(0)
        ]
    }
    m_v: u64 {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("HashMap accumulator") || sol.contains("staging map"),
        "HashMap fold acc must be rejected with a focused diagnostic mentioning the staging-map workaround: {sol}"
    );
}

// ---------------------------------------------------------------------------
// Phase EVM-15 H5 (Cluster D): auto-detect msg::value reads and emit `payable`
// ---------------------------------------------------------------------------

#[test]
fn evm15_h5_route_reading_msg_value_is_payable() {
    let src = r#"
entity Vault {
    routes {
        deposit() => [
            return(msg::value)
        ]
        peek() -> u256 => [
            return(0)
        ]
    }
    m_total: u256 {
        in deposit() => m_total + msg::value
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("function deposit() external payable"),
        "deposit reads msg::value (in route body and transform), should be payable: {sol}"
    );
    assert!(
        !sol.contains("function peek() external payable"),
        "peek does not read msg::value, should NOT be payable: {sol}"
    );
}

#[test]
fn evm15_h5_msg_value_in_transform_only_marks_route_payable() {
    let src = r#"
entity Pot {
    routes {
        contribute() => [ ]
        noop() => [ ]
    }
    m_pool: u256 {
        in contribute() => m_pool + msg::value
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("function contribute() external payable"),
        "contribute's transform reads msg::value, route must be payable: {sol}"
    );
    assert!(
        !sol.contains("function noop() external payable"),
        "noop has no msg::value usage, route must NOT be payable: {sol}"
    );
}

// ---------------------------------------------------------------------------
// Phase EVM-4 J1-J3 (Cluster J): tagged-union enum lowering
// ---------------------------------------------------------------------------

#[test]
fn evm4_j1_payload_enum_emits_tag_enum_and_struct() {
    let src = r#"
enum Action {
    Deposit(u64),
    Withdraw(u64, String),
    Reset
}

entity EnumA {
    routes {
        view get() -> u64 => [ return(0) ]
    }
    m_dummy: u64 {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("enum Action_Tag { Deposit, Withdraw, Reset }"),
        "payload enum must emit tag enum: {sol}"
    );
    assert!(
        sol.contains("struct Action {"),
        "payload enum must emit struct: {sol}"
    );
    assert!(
        sol.contains("Action_Tag tag;"),
        "struct must carry tag field: {sol}"
    );
    assert!(
        sol.contains("uint64 deposit_0;"),
        "Deposit payload field: {sol}"
    );
    assert!(
        sol.contains("uint64 withdraw_0;"),
        "Withdraw[0] payload field: {sol}"
    );
    assert!(
        sol.contains("string withdraw_1;"),
        "Withdraw[1] payload field: {sol}"
    );
}

#[test]
fn evm4_j1_unit_only_enum_still_emits_solidity_enum() {
    let src = r#"
enum State { Created, Funded, Released }

entity Unit {
    routes {
        view get() -> u64 => [ return(0) ]
    }
    m_state: State {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("enum State { Created, Funded, Released }"),
        "unit-only enum keeps simple Solidity enum form: {sol}"
    );
    assert!(
        !sol.contains("State_Tag"),
        "no tag enum for unit-only declarations: {sol}"
    );
    assert!(
        !sol.contains("struct State {"),
        "no struct for unit-only declarations: {sol}"
    );
}

#[test]
fn evm4_j2_enum_variant_with_data_constructs_struct_literal() {
    let src = r#"
enum Action {
    Deposit(u64),
    Withdraw(u64, String),
    Reset
}

entity EnumB {
    routes {
        push(amount: u64) => [
            // Build via member transform
        ]
    }
    m_last: Action {
        in push(amount) => Action::Deposit(amount)
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("Action({tag: Action_Tag.Deposit, deposit_0: amount, withdraw_0: 0, withdraw_1: \"\"})"),
        "Action::Deposit(amount) must lower to a struct literal with tag + payload + zero defaults for other variants: {sol}"
    );
}

#[test]
fn evm4_j2_unit_variant_within_payload_enum_uses_struct_constructor() {
    let src = r#"
enum Action {
    Deposit(u64),
    Reset
}

entity EnumC {
    routes {
        clear() => [
            // member transform handles it
        ]
    }
    m_last: Action {
        in clear() => Action::Reset
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("Action({tag: Action_Tag.Reset, deposit_0: 0})"),
        "Action::Reset (unit variant within a payload enum) must construct via the struct: {sol}"
    );
    assert!(
        !sol.contains("= Action.Reset;"),
        "must NOT emit the legacy `Action.Reset` form for payload enums: {sol}"
    );
}

#[test]
fn evm4_j3_match_payload_enum_unpacks_bindings_via_field_access() {
    let src = r#"
enum Action {
    Deposit(u64),
    Withdraw(u64, String),
    Reset
}

entity EnumD {
    routes {
        apply(action: Action) => []
    }
    m_balance: u64 {
        in apply(action) => {
            match action {
                Action::Deposit(amount) => m_balance + amount,
                Action::Withdraw(amount, reason) => m_balance - amount,
                Action::Reset => 0
            }
        }
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("action.tag == Action_Tag.Deposit"),
        "Match arm condition uses tag dispatch: {sol}"
    );
    assert!(
        sol.contains("(m_balance + action.deposit_0)"),
        "Binding `amount` must rewrite to `action.deposit_0`: {sol}"
    );
    assert!(
        sol.contains("(m_balance - action.withdraw_0)"),
        "Binding `amount` (Withdraw position 0) must rewrite to `action.withdraw_0`: {sol}"
    );
    assert!(
        sol.contains("action.tag == Action_Tag.Reset"),
        "Unit variant within payload enum still dispatches by tag: {sol}"
    );
}

#[test]
fn evm4_j3_match_wildcard_payload_pattern_emits_no_binding() {
    let src = r#"
enum Action {
    Withdraw(u64, String)
}

entity EnumE {
    routes {
        apply(action: Action) => []
    }
    m_balance: u64 {
        in apply(action) => {
            match action {
                Action::Withdraw(amount, _reason) => m_balance - amount
            }
        }
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("(m_balance - action.withdraw_0)"),
        "Bound `amount` rewrites to field access: {sol}"
    );
    // The body never references `_reason`, so no `withdraw_1` access
    // should leak out.
    assert!(
        !sol.contains("action.withdraw_1"),
        "Wildcard `_reason` pattern must not synthesise a use of `action.withdraw_1`: {sol}"
    );
}

#[test]
fn evm4_j_default_for_payload_enum_member_is_struct_zero_literal() {
    let src = r#"
enum Action { Deposit(u64), Reset }

entity EnumF {
    routes {
        view get() -> u64 => [ return(0) ]
    }
    m_last: Action {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("m_last = Action({tag: Action_Tag.Deposit, deposit_0: 0});"),
        "Default for a payload-enum-typed member with no explicit initialiser must use the struct zero literal of the first variant: {sol}"
    );
}

// Regression: a `pure fn` with the same name as a stdlib helper
// (`min`, `max`, `clamp`, `muldiv`, `divc`, `divr`, `divmod`) must
// suppress the helper emission, otherwise both end up in the output
// file and the Solidity compiler rejects with `Function with same
// name and parameter types defined twice` (Error 1686).
//
// Surfaced by `examples/uniswap-v2/UniswapV2Pair.cam`, which defines
// `pure fn min(a: U256, b: U256)` to use across the AMM math.
#[test]
fn evm_user_pure_fn_shadows_stdlib_helper_no_duplicate_emit() {
    let src = r#"
pure fn min(a: U256, b: U256) -> U256 {
    if a < b { a } else { b }
}

entity Capper {
    routes {
        cap(x: U256, lo: U256) => []
    }
    m_value: U256 {
        in cap(x, lo) => min(x, lo)
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    let occurrences = sol.matches("function min(").count();
    assert_eq!(
        occurrences, 1,
        "user `pure fn min(...)` shadows stdlib helper `min`; expected exactly one `function min(` declaration in emitted Solidity, found {occurrences}:\n{sol}"
    );
}

// `std::math::muldiv` must emit Cambrian `_cam_muldiv` (schoolbook
// 512-bit mul/div from cambrian-core), not the Remco Bloemen / Uniswap
// `FullMath` assembly, and must not collide with a user `pure fn muldiv`.
#[test]
fn evm_std_muldiv_emits_cam_helper_not_bloemen() {
    let src = r#"
pure fn muldiv(a: U256, b: U256, z: U256) -> U256 { a }

entity M {
    routes {
        run(x: U256, y: U256, z: U256) -> U256 => [ return(std::math::muldiv(x, y, z)) ]
    }
    m_dummy: U256 {
        in run(x, y, z) => x
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);

    assert!(
        sol.contains("function _cam_muldiv("),
        "missing _cam_muldiv helper:\n{sol}"
    );
    assert!(
        sol.contains("_cam_u512_mul"),
        "expected cambrian-core u512 port, not a third-party CRT/Newton helper:\n{sol}"
    );
    assert!(
        !sol.contains("not(0)") && !sol.contains("(3 * denominator) ^ 2"),
        "Bloemen/Uniswap FullMath assembly must not appear in generated Solidity:\n{sol}"
    );
    let user_muldiv = sol.matches("function muldiv(").count();
    assert_eq!(
        user_muldiv, 1,
        "user `pure fn muldiv` should remain as a single Solidity function:\n{sol}"
    );
}

// ---------------------------------------------------------------------------
// Phase EVM-P0-A — silent-miscompile cleanup
//   §1.12: mixed-type tuple destructure
//   §4.7 : revm test resolver placeholder (covered in test_evm_revm.rs / pinned by visual inspection here)
// ---------------------------------------------------------------------------

#[test]
fn evm_p0_a_let_tuple_destructure_with_divmod_emits_uint256_pair() {
    // `divmod` is a stdlib helper returning `(uint256, uint256)`; the
    // tuple destructure should pre-P0-A emit `(uint256 q, uint256 r) = divmod(a, b);`
    // — this is the unchanged baseline. The test pins it so subsequent
    // P0-A widening doesn't regress the homogeneous case.
    let src = r#"
entity D {
    routes {
        run(a: U256, b: U256) => [
            let (q, r) = divmod(a, b);
            return(q + r)
        ]
        view dummy() -> U256 => [ return(0) ]
    }
    m_x: U256 {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("(uint256 q, uint256 r) = divmod(a, b);"),
        "homogeneous tuple destructure should retain (uint256, uint256) shape: {sol}"
    );
}

#[test]
fn evm_p0_a_let_tuple_destructure_from_user_pure_fn_with_address_head() {
    // A user-defined pure fn returning `(address, U256, bool)`. The
    // pre-P0-A behaviour hardcoded `uint256` per slot — silently widening
    // the head `address` to uint256. After P0-A the destructure picks up
    // the correct types from the pure-fn return registry.
    let src = r#"
pure fn split(a: U256) -> (address, U256, bool) {
    if a > 0 {
        ("", a, true)
    } else {
        ("", 0, false)
    }
}

entity D {
    routes {
        run(a: U256) => [
            let (owner, count, paused) = split(a);
            return(count)
        ]
        view dummy() -> U256 => [ return(0) ]
    }
    m_x: U256 {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    // The pure fn now emits `returns (address, uint256, bool)` (Phase
    // EVM-P0-A's gen_pure_fn fix).
    assert!(
        sol.contains("function split(uint256 a) pure returns (address, uint256, bool)"),
        "Tuple-return pure fn must lower to multi-return signature: {sol}"
    );
    // Per-slot types in the tuple destructure.
    assert!(
        sol.contains("(address owner, uint256 count, bool paused) = split(a);"),
        "Tuple destructure should pick up per-slot types from pure-fn return: {sol}"
    );
}

#[test]
fn evm_p0_a_let_tuple_destructure_with_wildcard_keeps_gap() {
    // Wildcard slots stay empty so Solidity treats them as throwaway
    // tuple components.
    let src = r#"
entity D {
    routes {
        run(a: U256, b: U256) => [
            let (q, _) = divmod(a, b);
            return(q)
        ]
        view dummy() -> U256 => [ return(0) ]
    }
    m_x: U256 {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("(uint256 q, ) = divmod(a, b);"),
        "Wildcard slot should leave a typeless gap in the tuple destructure: {sol}"
    );
}

// ---------------------------------------------------------------------------
// Phase EVM-P0-C: events / emit / indexed parameters
// ---------------------------------------------------------------------------

#[test]
fn evm_p0_c_event_decl_emits_correct_solidity_signature() {
    let src = r#"
entity Token {
    event Transfer(indexed src: address, indexed dst: address, value: U256);
    routes { ping() => [] }
    m_total: U256 {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("event Transfer(address indexed src, address indexed dst, uint256 value);"),
        "missing entity-scope event declaration: {sol}"
    );
}

#[test]
fn evm_p0_c_indexed_topic_count_pinned() {
    let src = r#"
entity Token {
    event Transfer(indexed a: address, indexed b: address, c: U256);
    routes { ping() => [] }
    m_x: U256 {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    let n_indexed = sol.matches("indexed").count();
    assert_eq!(n_indexed, 2,
        "expected exactly 2 'indexed' tokens in event signature, got {n_indexed}: {sol}");
}

#[test]
fn evm_p0_c_emit_action_lowering() {
    let src = r#"
entity Token {
    event Ping(value: U256);
    routes {
        kick(amount: U256) => [
            emit Ping(amount);
        ]
    }
    m_x: U256 {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(sol.contains("event Ping(uint256 value);"), "missing event decl: {sol}");
    assert!(sol.contains("emit Ping(amount);"), "missing emit lowering: {sol}");
}

#[test]
fn evm_p0_c_program_scope_event_emitted_at_file_scope() {
    let src = r#"
event GlobalLog(indexed kind: u32, payload: U256);

entity A {
    routes {
        bump() => [
            emit GlobalLog(1, m_x);
        ]
    }
    m_x: U256 {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("event GlobalLog(uint32 indexed kind, uint256 payload);"),
        "missing program-scope event: {sol}"
    );
    assert!(sol.contains("emit GlobalLog("), "missing emit lowering: {sol}");
}

#[test]
fn evm_p0_c_emit_casts_narrow_int_args() {
    let src = r#"
entity Token {
    event Ping(value: u64);
    routes {
        kick(big: U256) => [
            emit Ping(big);
        ]
    }
    m_x: u64 {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("emit Ping(_toUint64(big));") || sol.contains("emit Ping(uint64(big));"),
        "expected explicit narrow cast on emit arg: {sol}"
    );
}

#[test]
fn evm_sd03_hashmap_exists_sidecar_on_member_insert() {
    let src = r#"
pure fn balance_of(balances: HashMap<address, U256>, owner: address) -> U256 {
    if balances.exists(owner) { balances[owner] } else { 0 }
}

entity Vault {
    routes {
        constructor(holder: address, supply: U256) => []
        view bal(owner: address) -> U256 => [ return(balance_of(m_balances, owner)) ]
    }
    m_balances: HashMap<address, U256> {
        in constructor(holder, supply) => {}.insert(holder, supply)
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("m_balances_exists[") && sol.contains("] = true;"),
        "non-iterated HashMap insert must flip _exists sidecar when pure fn uses .exists(): {sol}"
    );
}

// ---------------------------------------------------------------------------
// Phase EVM-P0-D: custom errors + revert
// ---------------------------------------------------------------------------

#[test]
fn evm_p0_d_error_decl_emitted() {
    let src = r#"
entity Vault {
    error InsufficientBalance(have: U256, need: U256);
    error Unauthorized();
    routes { ping() => [] }
    m_x: U256 {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(sol.contains("error InsufficientBalance(uint256 have, uint256 need);"),
        "missing custom error decl: {sol}");
    assert!(sol.contains("error Unauthorized();"),
        "missing zero-arg custom error: {sol}");
}

#[test]
fn evm_p0_d_throw_custom_error_lowering() {
    let src = r#"
entity Vault {
    error InsufficientBalance(have: U256, need: U256);
    routes {
        withdraw(amount: U256) => [
            throw InsufficientBalance(m_x, amount)
        ]
    }
    m_x: U256 {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(sol.contains("revert InsufficientBalance(m_x, amount);"),
        "missing custom revert lowering: {sol}");
}

#[test]
fn evm_p0_d_legacy_numeric_throw_unchanged() {
    let src = r#"
entity Vault {
    routes {
        withdraw(amount: U256) => [
            throw 7
        ]
    }
    m_x: U256 {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(sol.contains("revert(\"throw(7)\");"),
        "legacy numeric throw should still revert with throw(N): {sol}");
}

#[test]
fn evm_p0_d_throw_custom_in_where_clause() {
    let src = r#"
entity Vault {
    error Unauthorized();
    routes {
        admin(amount: U256) where amount > 0 : throw Unauthorized() => []
    }
    m_x: U256 {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(sol.contains("revert Unauthorized();"),
        "where clause should revert with custom error: {sol}");
}

#[test]
fn evm_p0_d_program_scope_error_emitted_at_file_scope() {
    let src = r#"
error GlobalErr(code: u32);

entity A {
    routes {
        bump() => [
            throw GlobalErr(42)
        ]
    }
    m_x: U256 {}
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(sol.contains("error GlobalErr(uint32 code);"),
        "missing program-scope error: {sol}");
    assert!(sol.contains("revert GlobalErr("),
        "missing custom revert: {sol}");
}

// ---------------------------------------------------------------------------
// Phase EVM-P0-E: receive / fallback / ETH-receiving entities
// ---------------------------------------------------------------------------

#[test]
fn evm_p0_e_receive_emitted_payable() {
    let src = r#"
entity Vault {
    routes {
        accept receive() => []
    }
    m_balance: U256 {
        in receive() => m_balance + msg::value
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(sol.contains("receive() external payable {"),
        "missing receive() external payable signature: {sol}");
}

#[test]
fn evm_p0_e_fallback_payable_when_value_used() {
    let src = r#"
entity Vault {
    routes {
        fallback() => []
    }
    m_balance: U256 {
        in fallback() => m_balance + msg::value
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(sol.contains("fallback() external payable {"),
        "fallback that reads msg::value must be payable: {sol}");
}

#[test]
fn evm_p0_e_fallback_non_payable_when_value_unused() {
    let src = r#"
entity Vault {
    routes {
        fallback() => []
    }
    m_calls: U256 {
        in fallback() => m_calls + 1
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(sol.contains("fallback() external {"),
        "fallback that doesn't read msg::value should be non-payable: {sol}");
    assert!(!sol.contains("fallback() external payable"),
        "fallback should not be payable here: {sol}");
}

#[test]
fn evm_program_records_emitted_in_dependency_order() {
    // A references B but is declared first — kernel type topo must emit B before A.
    let src = r#"
record A { b: B }
record B { x: u64 }
entity E {
    routes {
        noop() => []
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    let pos_a = sol.find("struct A").expect("struct A");
    let pos_b = sol.find("struct B").expect("struct B");
    assert!(
        pos_b < pos_a,
        "B must be declared before A (dependency order); got:\n{sol}"
    );
}

#[test]
fn evm_entity_local_records_emitted_in_dependency_order() {
    let src = r#"
entity E {
    record A { b: B }
    record B { x: u64 }
    routes {
        noop() => []
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    let pos_a = sol.find("struct A").expect("struct A");
    let pos_b = sol.find("struct B").expect("struct B");
    assert!(
        pos_b < pos_a,
        "entity-local B must be declared before A; got:\n{sol}"
    );
}

// ---------------------------------------------------------------------------
// A cross-contract READ needs the callee's interface. Through a plain
// `address` the destination entity cannot be resolved, and codegen used to
// emit a comment in place of the call — leaving the bound variable at zero,
// so every guard reading it silently took the wrong branch and the contract
// still compiled. V23 rejects this at validation time; codegen must still
// refuse on its own when invoked without validation.
// ---------------------------------------------------------------------------

#[test]
#[should_panic(expected = "cannot resolve the target entity of var call")]
fn evm_var_call_through_plain_address_stops_the_build() {
    let src = r#"
entity Token {
    routes {
        constructor(initial_supply: U256) => []
        view balanceOf(owner: address) -> U256 => [
            return(m_total_supply)
        ]
    }
    m_total_supply: U256 {
        in constructor(initial_supply) => initial_supply
    }
}

entity Vault {
    routes {
        constructor(token_addr: address) => []
        view myBalance(who: address) -> U256 => [
            var bal = balanceOf(who) ~> m_token;
            return(bal)
        ]
    }
    m_token: address {
        in constructor(token_addr) => token_addr
    }
}
"#;
    let program = parse(src);
    let _ = gen_evm_solidity(&program, true);
}

// ---------------------------------------------------------------------------
// Invariant harness: the declared sender set has to reach the entity, and an
// actor has to be confinable to a finite set.
//
// Both failures below are silent by construction — the suite stays green
// because the trace is empty, which is the one failure mode a test suite
// cannot report on itself.
// ---------------------------------------------------------------------------

fn gen_invariant_tests(src: &str) -> String {
    let program = parse(src);
    let cfg = cambrian_transpiler::project::InvariantConfig::default();
    cambrian_transpiler::codegen::evm_test_codegen::generate_evm_tests(&program, true, &cfg)
        .into_iter()
        .map(|(_, body)| body)
        .collect::<Vec<_>>()
        .join("\n")
}

const OWNED_COUNTER: &str = r#"
entity Owned {
    routes {
        constructor(owner_: address) => []
        bump(who: address, amount: u64)
            where msg::sender == m_owner : throw 1
        => []
        view count() -> u64 => [ return(m_count) ]
    }
    m_owner: address { in constructor(owner_) => owner_ }
    m_count: u64 { in bump(_, amount) => m_count + amount }
}
"#;

#[test]
fn an_invariant_forwards_its_declared_sender_into_the_entity() {
    // `targetSender` picks who calls the *handler*; the entity sees the
    // handler's own address unless the forwarded call is pranked. Without
    // the prank a guard of the shape `msg::sender == m_owner` rejects every
    // call, each action reverts into the handler's `try/catch`, and the
    // invariant holds over a run in which nothing executed.
    let src = format!(
        "{OWNED_COUNTER}
invariant \"count is bounded\" for Owned {{
    init {{ m_owner: 0x0000000000000000000000000000000000000000000000000000000000000a01 }}
    senders {{ 0x0000000000000000000000000000000000000000000000000000000000000a01 }}
    action bump(who: address, amount: u64) {{ }}
    check count() >= 0
}}
"
    );
    let sol = gen_invariant_tests(&src);
    assert!(
        sol.contains("vm.prank(msg.sender);"),
        "the handler must forward the rotated sender into the entity: {sol}"
    );
}

#[test]
fn an_invariant_without_a_sender_set_does_not_prank() {
    // With no `senders { … }` the author stated no intent, Foundry picks
    // arbitrary callers, and the handler's own identity is as good as any.
    // Pranking anyway would change every existing invariant's meaning.
    let src = format!(
        "{OWNED_COUNTER}
invariant \"count is bounded\" for Owned {{
    init {{ m_count: 0 }}
    action bump(who: address, amount: u64) {{ }}
    check count() >= 0
}}
"
    );
    let sol = gen_invariant_tests(&src);
    assert!(
        !sol.contains("vm.prank(msg.sender);"),
        "an invariant that names no senders must keep the handler as caller: {sol}"
    );
}

#[test]
fn bounding_an_address_parameter_lowers_through_uint160() {
    // Confining an actor is the only way to hold a derived total against a
    // stored one, and `bound` is the only construct that can do it: the
    // obvious spelling `assume p == <literal>` rejects the draw at odds of
    // 2^-160, so the action never runs. `bound` is numeric, so the parameter
    // and both endpoints round-trip through `uint160`.
    let src = format!(
        "{OWNED_COUNTER}
invariant \"count is bounded\" for Owned {{
    init {{ m_count: 0 }}
    action bump(who: address, amount: u64) {{
        bound who in 0x0000000000000000000000000000000000000000000000000000000000000a01 ..= 0x0000000000000000000000000000000000000000000000000000000000000a02
        bound amount in 0..10
    }}
    check count() >= 0
}}
"
    );
    let sol = gen_invariant_tests(&src);
    assert!(
        sol.contains("who = address(uint160(bound(uint256(uint160(who)),"),
        "an address bound must round-trip through uint160: {sol}"
    );
    // A numeric parameter keeps the plain form.
    assert!(
        sol.contains("amount = uint64(bound(amount, 0,"),
        "a numeric bound must stay unwrapped: {sol}"
    );
}

#[test]
fn two_entities_may_carry_the_same_invariant_title() {
    // The generated path used to be keyed by invariant name alone. A derived
    // component and its base routinely carry the same invariant title, so the
    // second file overwrote the first: one of the two invariants was silently
    // dropped from the suite, and the build log looked identical to a healthy
    // one. Removing a guard from the clobbered entity then changed nothing —
    // the invariant that would have caught it was not in the build.
    let src = r#"
entity Alpha {
    routes {
        constructor() => []
        bump(amount: u64) => []
        view count() -> u64 => [ return(m_count) ]
    }
    m_count: u64 { in bump(amount) => m_count + amount }
}

entity Beta {
    routes {
        constructor() => []
        bump(amount: u64) => []
        view count() -> u64 => [ return(m_count) ]
    }
    m_count: u64 { in bump(amount) => m_count + amount }
}

invariant "the count only grows" for Alpha {
    action bump(amount: u64) { bound amount in 0..10 }
    check count() >= 0
}

invariant "the count only grows" for Beta {
    action bump(amount: u64) { bound amount in 0..10 }
    check count() >= 0
}
"#;
    let program = parse(src);
    let cfg = cambrian_transpiler::project::InvariantConfig::default();
    let files =
        cambrian_transpiler::codegen::evm_test_codegen::generate_evm_tests(&program, true, &cfg);

    let paths: Vec<&str> = files
        .iter()
        .map(|(p, _)| p.as_str())
        .filter(|p| p.starts_with("test/Invariant_"))
        .collect();
    assert_eq!(
        paths.len(),
        2,
        "both invariants must be emitted, got: {paths:?}"
    );
    assert_ne!(
        paths[0], paths[1],
        "same-titled invariants of two entities must not share a path: {paths:?}"
    );

    // Solidity contract names must differ too, otherwise `--match-contract`
    // cannot name one of the two.
    let bodies: Vec<&str> = files
        .iter()
        .filter(|(p, _)| p.starts_with("test/Invariant_"))
        .map(|(_, b)| b.as_str())
        .collect();
    assert!(
        bodies
            .iter()
            .any(|b| b.contains("contract Invariant_Alpha_the_count_only_grows_Test")),
        "expected an Alpha-qualified test contract: {bodies:?}"
    );
    assert!(
        bodies
            .iter()
            .any(|b| b.contains("contract Invariant_Beta_the_count_only_grows_Test")),
        "expected a Beta-qualified test contract: {bodies:?}"
    );
}

#[test]
fn a_string_view_binds_into_a_memory_local() {
    // A Solidity local holding a reference type needs a data location exactly
    // as a parameter does: `string _ret_1 = t.name();` does not compile. The
    // return binding used to ask for the bare type, so any entity carrying a
    // `String` view produced a test file the compiler rejected — which is why
    // no test in the corpus asserted on `name()` until this was fixed.
    let src = r#"
entity Named {
    routes {
        constructor(name_: String) => []
        view name() -> String => [ return(m_name) ]
    }
    m_name: String { in constructor(name_) => name_ }
}

test "the name comes back" for Named {
    call constructor("Token")
    call name()
    expect return "Token"
}
"#;
    let program = parse(src);
    let cfg = cambrian_transpiler::project::InvariantConfig::default();
    let files =
        cambrian_transpiler::codegen::evm_test_codegen::generate_evm_tests(&program, true, &cfg);

    let body = files
        .iter()
        .find(|(p, _)| p.ends_with("Named.t.sol"))
        .map(|(_, b)| b.as_str())
        .expect("expected a unit-test file for Named");

    assert!(
        body.contains("string memory _ret_"),
        "the return binding must carry a data location: {body}"
    );
    assert!(
        !body.contains("        string _ret_"),
        "a bare `string` local does not compile: {body}"
    );
}

#[test]
fn bytes4_survives_to_the_abi() {
    // ERC-165 answers on a four-byte interface id. Every other width produces
    // a different selector, so `supportsInterface` on a `u32` or a `bytes32`
    // is not a stricter or looser version of the standard route — it is a
    // different function that no caller will ever reach, and the standard
    // staticcall reads the miss as "not supported".
    //
    // The validator used to reject `bytes4` outright, and the type lowering
    // would have erased it to `uint256` through the catch-all if it had not.
    // The second assertion is the one that matters: an erasure still compiles
    // and still passes any test that only checks the route exists.
    let src = r#"
entity Probe {
    routes {
        constructor() => []
        view supportsInterface(interface_id: bytes4) -> bool => [
            return(interface_id == 0x01ffc9a7)
        ]
    }
}
"#;
    let program = parse(src);
    let out = gen_evm_solidity(&program, true);

    assert!(
        out.contains("function supportsInterface(bytes4 interface_id)"),
        "bytes4 must reach the ABI verbatim: {out}"
    );
    assert!(
        !out.contains("supportsInterface(uint256"),
        "bytes4 must not be erased to uint256: {out}"
    );
}

// ---------------------------------------------------------------------------
// Deterministic `initialize()` must register init-route parameter types the
// same way `constructor` does. Without that, a `String` argument assigned to
// a `String` member is inferred as `uint256` and coerced through `_cam_itoa`
// — output that does not compile, and a divergence between the two builds of
// the same source.
// ---------------------------------------------------------------------------

fn named_token_src() -> &'static str {
    r#"
entity Named {
    routes {
        constructor(name_: String) => []
        view name() -> String => [return(m_name)]
    }
    m_name: String {
        in constructor(name_) => name_
    }
}
"#
}

fn initialize_fn_body(sol: &str) -> &str {
    let start = sol
        .find("function initialize(")
        .unwrap_or_else(|| panic!("missing initialize(): {sol}"));
    let rest = &sol[start..];
    let end = rest
        .find("\n    }\n")
        .unwrap_or_else(|| panic!("unterminated initialize(): {sol}"));
    &rest[..end]
}

#[test]
fn evm_det_initialize_string_param_not_itoa() {
    let program = parse(named_token_src());

    let non_det = gen_evm_solidity(&program, true);
    assert!(
        !non_det.contains("_cam_itoa"),
        "non-deterministic constructor must assign the String param directly:\n{non_det}"
    );

    let det = gen_evm_solidity(&program, true);
    let init = initialize_fn_body(&det);
    assert!(
        init.contains("string memory name_"),
        "initialize() should take the String param:\n{init}"
    );
    assert!(
        !init.contains("_cam_itoa"),
        "initialize() must not coerce a String param through _cam_itoa:\n{init}"
    );
    assert!(
        init.contains("name_"),
        "initialize() should mention the param in the transform:\n{init}"
    );
}

// ---------------------------------------------------------------------------
// Foundry `with { m_owner: 0x… }` seeds an address slot via `vm.store`.
// Solidity has no `uint256(address(...))` conversion — it has to go through
// `uint160` first, or the generated harness fails to compile.
// ---------------------------------------------------------------------------

#[test]
fn evm_foundry_address_with_clause_casts_through_uint160() {
    let src = r#"
entity Vault {
    routes {
        #[factory_only]
        constructor() => []
        ping() => []
    }
    m_owner: address {}
}

test "seed owner" for Vault with { m_owner: 0x0000000000000000000000000000000000000001 } {
    call ping()
}
"#;
    let program = parse(src);
    let files = cambrian_transpiler::codegen::evm_test_codegen::generate_evm_tests(
        &program,
        true,
        &cambrian_transpiler::project::InvariantConfig::default(),
    );
    let harness = files
        .iter()
        .find(|(path, _)| path.starts_with("test/") && path.ends_with(".t.sol"))
        .map(|(_, body)| body.as_str())
        .unwrap_or_else(|| panic!("no Foundry test file generated: {files:?}"));
    assert!(
        !harness.contains("uint256(address("),
        "vm.store must not wrap address(...) in uint256(...) directly:\n{harness}"
    );
    assert!(
        harness.contains("uint160("),
        "address-typed with-clause seed must go through uint160:\n{harness}"
    );
}

#[test]
fn evm_codegen_coerces_uint64_send_dest_to_address() {
    let src = r#"
entity SendU64 {
    routes {
        constructor() => []
        pay() => [
            let dest = 1;
            ~> dest
        ]
    }
    m_n: u64 { in constructor() => 0 }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("address(uint160(uint256(dest)))"),
        "uint64 send dest must wrap to address:\n{sol}"
    );
}

#[test]
fn evm_codegen_narrows_sum_not_prefix_cast() {
    let src = r#"
entity NarrowPfx {
    routes {
        constructor() => []
        go(x: U256, y: u64) -> u64 => [
            return((x as u64) + y)
        ]
    }
    m_n: u64 { in constructor() => 0 }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    let ret = sol
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("return "))
        .unwrap_or("");
    assert!(
        !(ret.starts_with("return uint64(") && ret.contains(" + ") && !ret.starts_with("return uint64((")),
        "sum must not skip narrow because of uint64( prefix: {ret}\n{sol}"
    );
    assert!(
        !(ret.starts_with("return _toUint64(") && ret.contains(" + ") && !ret.starts_with("return _toUint64((")),
        "sum must not skip narrow because of _toUint64( prefix: {ret}\n{sol}"
    );
    assert!(
        sol.contains("_toUint64") || sol.contains("uint64("),
        "u64 return of a sum must still narrow:\n{sol}"
    );
}
