// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Phase Library-4: `@solidity_import("...")` annotation on
//! `extern entity` declarations. When set, the EVM emitter
//! prepends a `import "<path>";` directive to the generated `.sol`
//! and suppresses its synthetic `interface IName { ... }` block.
//! The author is responsible for keeping the imported Solidity
//! surface in sync with the Cambrian `extern entity` declaration.

use cambrian_transpiler::ProgramParser;
use cambrian_transpiler::codegen::gen_evm_solidity;

fn parse(src: &str) -> cambrian_transpiler::ast::Program {
    let mut program = ProgramParser::new()
        .parse(src)
        .unwrap_or_else(|e| panic!("Parse error: {e}"));
    cambrian_transpiler::ast::normalize_program_types(&mut program);
    program
}

#[test]
fn solimport_parser_accepts_annotation() {
    let src = r#"
@solidity_import("@openzeppelin/contracts/token/ERC20/IERC20.sol")
extern entity IERC20 {
    route transfer(to: address, amount: U256) -> bool;
    view route balanceOf(who: address) -> U256;
}
"#;
    let program = ProgramParser::new()
        .parse(src)
        .unwrap_or_else(|e| panic!("Parse error: {e}"));
    let ext = program
        .extern_entities
        .iter()
        .find(|e| e.name == "IERC20")
        .expect("missing IERC20");
    assert_eq!(
        ext.solidity_import.as_deref(),
        Some("@openzeppelin/contracts/token/ERC20/IERC20.sol"),
        "solidity_import must round-trip through the parser",
    );
    assert_eq!(
        ext.routes.len(),
        2,
        "annotation must not eat the `extern entity` body",
    );
}

#[test]
fn solimport_emits_solidity_import_line() {
    let src = r#"
@solidity_import("@openzeppelin/contracts/token/ERC20/IERC20.sol")
extern entity IERC20 {
    route transfer(to: address, amount: U256) -> bool;
    view route balanceOf(who: address) -> U256;
}

entity Caller {
    routes {
        constructor(t: Address<IERC20>) => []
        pay(amount: U256) => [
            transfer(m_token, amount) ~> m_token
        ]
    }
    m_token: Address<IERC20> {
        in constructor(t) => t
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("import \"@openzeppelin/contracts/token/ERC20/IERC20.sol\";"),
        "missing Solidity import line: {sol}",
    );
}

#[test]
fn solimport_suppresses_synthetic_interface() {
    // When the extern entity is satisfied by an external Solidity
    // import, the synthetic `interface IName { ... }` must NOT be
    // emitted. Otherwise solc rejects with a duplicate-symbol error.
    let src = r#"
@solidity_import("@openzeppelin/contracts/token/ERC20/IERC20.sol")
extern entity IERC20 {
    route transfer(to: address, amount: U256) -> bool;
}

entity Caller {
    routes {
        constructor(t: Address<IERC20>) => []
        pay(amount: U256) => [
            transfer(m_token, amount) ~> m_token
        ]
    }
    m_token: Address<IERC20> {
        in constructor(t) => t
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(
        !sol.contains("interface IIERC20"),
        "must not double-prefix the synthetic stub: {sol}",
    );
    // The synthetic interface starts with `interface I<name> {`. We
    // also reject the un-prefixed form `interface IERC20 {` because
    // the import already brings IERC20 into scope.
    assert!(
        !sol.contains("interface IERC20 {"),
        "synthetic interface must be suppressed when @solidity_import is set: {sol}",
    );
}

#[test]
fn solimport_call_sites_skip_synthetic_i_prefix() {
    // When `@solidity_import` is set, call sites must NOT prepend
    // the synthetic `I` prefix — the author has chosen the extern
    // entity's name to match the symbol exported by the imported
    // `.sol`. So `extern entity IERC20 { ... }` casts to
    // `IERC20(addr).transfer(...)`, not `IIERC20(addr).transfer(...)`.
    let src = r#"
@solidity_import("@openzeppelin/contracts/token/ERC20/IERC20.sol")
extern entity IERC20 {
    route transfer(to: address, amount: U256) -> bool;
}

entity Caller {
    routes {
        constructor(t: Address<IERC20>) => []
        pay(amount: U256) => [
            transfer(m_token, amount) ~> m_token
        ]
    }
    m_token: Address<IERC20> {
        in constructor(t) => t
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    assert!(
        sol.contains("IERC20(") && sol.contains(".transfer("),
        "call site must cast through IERC20(addr).transfer(...): {sol}",
    );
    assert!(
        !sol.contains("IIERC20("),
        "must not double-prefix the cast when @solidity_import is set: {sol}",
    );
}

#[test]
fn solimport_no_annotation_still_emits_synthetic_interface() {
    // Negative control: extern entity without the annotation must
    // still produce the synthetic interface (existing EVM-6 M1
    // behaviour is preserved).
    let src = r#"
extern entity Token {
    route transfer(to: address, amount: U256) -> bool;
}

entity Caller {
    routes {
        constructor(t: Address<Token>) => []
        pay(amount: U256) => [
            transfer(m_token, amount) ~> m_token
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
        "synthetic interface missing for non-annotated extern entity: {sol}",
    );
    assert!(
        !sol.contains("import \""),
        "must not emit a Solidity import line when no annotation is present: {sol}",
    );
}

#[test]
fn solimport_multiple_imports_are_deduped_and_sorted() {
    let src = r#"
@solidity_import("@openzeppelin/contracts/token/ERC20/IERC20.sol")
extern entity IERC20 {
    route transfer(to: address, amount: U256) -> bool;
}

@solidity_import("@openzeppelin/contracts/access/Ownable.sol")
extern entity Ownable {
    view route owner() -> address;
}

entity Caller {
    routes {
        constructor(t: Address<IERC20>, o: Address<Ownable>) => []
    }
    m_token: Address<IERC20> {
        in constructor(t) => t
    }
    m_owner: Address<Ownable> {
        in constructor(o) => o
    }
}
"#;
    let program = parse(src);
    let sol = gen_evm_solidity(&program, true);
    let ownable_pos = sol
        .find("import \"@openzeppelin/contracts/access/Ownable.sol\";")
        .expect(&format!("missing Ownable import: {sol}"));
    let erc20_pos = sol
        .find("import \"@openzeppelin/contracts/token/ERC20/IERC20.sol\";")
        .expect(&format!("missing IERC20 import: {sol}"));
    assert!(
        ownable_pos < erc20_pos,
        "imports must be sorted (Ownable < IERC20 alphabetically): {sol}",
    );
}

#[test]
fn solimport_foundry_remappings_serialise() {
    use cambrian_transpiler::codegen::evm_test_codegen::generate_foundry_toml;
    use cambrian_transpiler::project::FoundryConfig;

    let cfg = FoundryConfig {
        remappings: Some(vec![
            "@openzeppelin/=lib/openzeppelin-contracts/".to_string(),
            "forge-std/=lib/forge-std/src/".to_string(),
        ]),
        ..Default::default()
    };
    let toml = generate_foundry_toml(Some(&cfg), None, None);
    assert!(
        toml.contains("remappings = [\"@openzeppelin/=lib/openzeppelin-contracts/\", \"forge-std/=lib/forge-std/src/\"]"),
        "remappings array missing from default profile: {toml}",
    );
    // Tiered profiles inherit the same remappings.
    assert!(
        toml.matches("remappings = [\"@openzeppelin/=lib/openzeppelin-contracts/\"")
            .count()
            >= 2,
        "tiered profiles must inherit remappings: {toml}",
    );
}

#[test]
fn solimport_foundry_remappings_absent_when_unset() {
    use cambrian_transpiler::codegen::evm_test_codegen::generate_foundry_toml;

    let toml = generate_foundry_toml(None, None, None);
    assert!(
        !toml.contains("remappings ="),
        "must not emit a `remappings =` line when none configured: {toml}",
    );
}
