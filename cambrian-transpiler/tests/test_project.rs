// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

use std::path::Path;

use cambrian_transpiler::codegen::{EvmSolidityBackend, OutputBackend};
use cambrian_transpiler::project::{Project, ProjectError};

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("contracts")
        .join(name)
}

// ===========================================================================
// YAML parsing
// ===========================================================================

#[test]
fn yaml_parses_valid_config() {
    let project = Project::load(&fixture("project.yaml")).unwrap();
    assert_eq!(project.config.name.as_deref(), Some("test-dapp"));
    assert_eq!(project.config.target, "evm");
    assert_eq!(project.config.sources.len(), 2);
    assert_eq!(project.config.output_dir, "build/");
    assert!(project.config.library_paths.is_empty());
    assert!(project.config.imports.is_empty());
}

#[test]
fn yaml_default_output_dir() {
    let yaml = "name: minimal\ntarget: evm\nsources:\n  - counter.cam\n";
    let dir = tempdir();
    let yaml_path = dir.join("minimal.yaml");
    std::fs::write(&yaml_path, yaml).unwrap();
    std::fs::copy(fixture("proj_counter.cam"), dir.join("counter.cam")).unwrap();

    let project = Project::load(&yaml_path).unwrap();
    assert_eq!(project.config.output_dir, "build/");
}

#[test]
fn yaml_name_defaults_to_dir_name() {
    let yaml = "target: evm\nsources:\n  - counter.cam\n";
    let dir = tempdir();
    let yaml_path = dir.join("project.yaml");
    std::fs::write(&yaml_path, yaml).unwrap();
    std::fs::copy(fixture("proj_counter.cam"), dir.join("counter.cam")).unwrap();

    let project = Project::load(&yaml_path).unwrap();
    assert!(project.config.name.is_none());
    let name = project.name();
    assert!(!name.is_empty());
}

#[test]
fn yaml_missing_file_is_error() {
    let yaml = "target: evm\nsources:\n  - nonexistent.cam\n";
    let dir = tempdir();
    let yaml_path = dir.join("bad.yaml");
    std::fs::write(&yaml_path, yaml).unwrap();

    let result = Project::load(&yaml_path);
    assert!(result.is_err());
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("nonexistent.cam"),
        "error should mention the missing file: {}",
        msg
    );
}

#[test]
fn yaml_empty_sources_is_error() {
    let yaml = "target: evm\nsources: []\n";
    let dir = tempdir();
    let yaml_path = dir.join("empty.yaml");
    std::fs::write(&yaml_path, yaml).unwrap();

    let result = Project::load(&yaml_path);
    assert!(result.is_err());
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("empty"),
        "error should say sources is empty: {}",
        msg
    );
}

// ===========================================================================
// Multi-file merging
// ===========================================================================

#[test]
fn merge_combines_entities_from_multiple_files() {
    let project = Project::load(&fixture("project.yaml")).unwrap();
    let entity_names: Vec<&str> = project
        .merged
        .entities
        .iter()
        .map(|e| e.name.as_str())
        .collect();
    assert!(
        entity_names.contains(&"Counter"),
        "merged should contain Counter"
    );
    assert!(
        entity_names.contains(&"Vault"),
        "merged should contain Vault"
    );
    assert_eq!(entity_names.len(), 2);
}

#[test]
fn merge_preserves_entity_members() {
    let project = Project::load(&fixture("project.yaml")).unwrap();
    let vault = project
        .merged
        .entities
        .iter()
        .find(|e| e.name == "Vault")
        .expect("Vault entity should exist");
    assert!(
        vault
            .members
            .iter()
            .any(|m| m.name == "m_id" && m.is_identity),
        "Vault should have identity member m_id"
    );
    assert!(
        vault.members.iter().any(|m| m.name == "m_balance"),
        "Vault should have member m_balance"
    );
}

#[test]
fn merge_per_program_data_preserved() {
    let project = Project::load(&fixture("project.yaml")).unwrap();
    assert_eq!(project.programs.len(), 2);
    let counter_prog = &project.programs[0].1;
    assert_eq!(counter_prog.entities.len(), 1);
    assert_eq!(counter_prog.entities[0].name, "Counter");
}

// ===========================================================================
// Conflict detection
// ===========================================================================

#[test]
fn merge_detects_duplicate_entity() {
    let result = Project::load(&fixture("conflict.yaml"));
    assert!(result.is_err());
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("duplicate entity"),
        "error should mention duplicate entity: {}",
        msg
    );
    assert!(
        msg.contains("Counter"),
        "error should mention Counter: {}",
        msg
    );
}

// ===========================================================================
// Project-level EVM codegen
// ===========================================================================

#[test]
fn evm_gen_project_produces_single_sol() {
    let project = Project::load(&fixture("project.yaml")).unwrap();
    let backend = EvmSolidityBackend {
        deterministic_addresses: true,
    };
    let files = backend.gen_project(&project);

    // The combined Solidity source lives in a single `_<project>_project.sol`
    // file. Per-entity stub files (`src/<Entity>.sol`) are also emitted so
    // that test imports of the form `../src/<Entity>.sol` resolve, but they
    // are thin re-export shims, not standalone contract sources.
    let sol_files: Vec<&(String, String)> =
        files.iter().filter(|(p, _)| p.ends_with(".sol")).collect();
    let combined: Vec<&&(String, String)> = sol_files
        .iter()
        .filter(|(p, _)| p.contains("_project.sol"))
        .collect();
    assert_eq!(
        combined.len(),
        1,
        "EVM should produce exactly one combined-project .sol file, got: {:?}",
        sol_files.iter().map(|(p, _)| p).collect::<Vec<_>>()
    );
    let (path, contents) = combined[0];
    assert!(path.ends_with(".sol"), "output should be .sol: {}", path);
    assert!(
        contents.contains("contract Counter"),
        "should contain Counter contract"
    );
    assert!(
        contents.contains("contract Vault"),
        "should contain Vault contract"
    );
    assert!(contents.contains("pragma solidity"), "should have pragma");
}

#[test]
fn evm_gen_project_file_named_after_project() {
    let project = Project::load(&fixture("project.yaml")).unwrap();
    let backend = EvmSolidityBackend {
        deterministic_addresses: true,
    };
    let files = backend.gen_project(&project);
    // The combined-project .sol file uses an `_<project>_project.sol`
    // suffix to dodge case-insensitive filesystem collisions when an
    // entity name matches the project name (e.g. project `governor` +
    // entity `Governor` would otherwise both want `src/governor.sol`).
    assert!(
        files.iter().any(|(p, _)| p == "src/_test-dapp_project.sol"),
        "EVM project output should be named after project at \
         src/_test-dapp_project.sol; got files: {:?}",
        files.iter().map(|(p, _)| p).collect::<Vec<_>>()
    );
}

// ===========================================================================
// Deterministic addresses codegen
// ===========================================================================

#[test]
fn det_yaml_parses_deterministic_toggle() {
    let project = Project::load(&fixture("det_project.yaml")).unwrap();
    assert_eq!(project.config.deterministic_addresses, Some(true));
}

#[test]
fn det_yaml_implicit_resolves_true_for_evm() {
    let dir = tempdir();
    let yaml_path = dir.join("implicit_det.yaml");
    std::fs::write(
        &yaml_path,
        "name: implicit-det\ntarget: evm\noutput_dir: build/\nsources:\n  - counter.cam\n",
    )
    .unwrap();
    std::fs::copy(fixture("proj_counter.cam"), dir.join("counter.cam")).unwrap();
    let project = Project::load(&yaml_path).unwrap();
    assert_eq!(project.config.deterministic_addresses, None);
    assert!(project.config.resolved_deterministic_addresses());
}

#[test]
fn det_factory_contract_generated() {
    let project = Project::load(&fixture("det_project.yaml")).unwrap();
    let backend = EvmSolidityBackend {
        deterministic_addresses: true,
    };
    let files = backend.gen_project(&project);
    let (_, code) = &files[0];
    assert!(
        code.contains("contract CambrianFactory"),
        "should contain factory contract"
    );
}

#[test]
fn det_factory_interface_generated() {
    let project = Project::load(&fixture("det_project.yaml")).unwrap();
    let backend = EvmSolidityBackend {
        deterministic_addresses: true,
    };
    let files = backend.gen_project(&project);
    let (_, code) = &files[0];
    assert!(
        code.contains("interface ICambrianFactory"),
        "should contain factory interface"
    );
}

#[test]
fn det_factory_has_deploy_and_predict() {
    let project = Project::load(&fixture("det_project.yaml")).unwrap();
    let backend = EvmSolidityBackend {
        deterministic_addresses: true,
    };
    let files = backend.gen_project(&project);
    let (_, code) = &files[0];
    assert!(
        code.contains("function deployCounter("),
        "should have deployCounter"
    );
    assert!(
        code.contains("function predictCounter("),
        "should have predictCounter"
    );
    assert!(
        code.contains("function deployVault("),
        "should have deployVault"
    );
    assert!(
        code.contains("function predictVault("),
        "should have predictVault"
    );
}

#[test]
fn det_entities_have_factory_immutable() {
    let project = Project::load(&fixture("det_project.yaml")).unwrap();
    let backend = EvmSolidityBackend {
        deterministic_addresses: true,
    };
    let files = backend.gen_project(&project);
    let (_, code) = &files[0];
    // Both Counter and Vault should have _factory immutable
    let counter_start = code.find("contract Counter").expect("Counter contract");
    let counter_end = code[counter_start..]
        .find("\ncontract ")
        .map(|i| counter_start + i)
        .unwrap_or(code.len());
    let counter_code = &code[counter_start..counter_end];
    assert!(
        counter_code.contains("address public immutable _factory"),
        "Counter should have _factory immutable"
    );
}

#[test]
fn det_vault_has_initialize() {
    let project = Project::load(&fixture("det_project.yaml")).unwrap();
    let backend = EvmSolidityBackend {
        deterministic_addresses: true,
    };
    let files = backend.gen_project(&project);
    let (_, code) = &files[0];
    // Vault has init route with non-identity param (initialBalance), needs initialize()
    let vault_start = code.find("contract Vault").expect("Vault contract");
    let vault_end = code[vault_start..]
        .find("\ncontract ")
        .map(|i| vault_start + i)
        .unwrap_or(code.len());
    let vault_code = &code[vault_start..vault_end];
    assert!(
        vault_code.contains("function initialize("),
        "Vault should have initialize()"
    );
    assert!(
        vault_code.contains("_initialized"),
        "Vault should have _initialized guard"
    );
}

#[test]
fn det_counter_no_initialize() {
    let project = Project::load(&fixture("det_project.yaml")).unwrap();
    let backend = EvmSolidityBackend {
        deterministic_addresses: true,
    };
    let files = backend.gen_project(&project);
    let (_, code) = &files[0];
    // Counter has no init route params, no initialize() needed
    let counter_start = code.find("contract Counter").expect("Counter contract");
    let counter_end = code[counter_start..]
        .find("\ncontract ")
        .map(|i| counter_start + i)
        .unwrap_or(code.len());
    let counter_code = &code[counter_start..counter_end];
    assert!(
        !counter_code.contains("function initialize("),
        "Counter should NOT have initialize()"
    );
    assert!(
        !counter_code.contains("_initialized"),
        "Counter should NOT have _initialized"
    );
}

#[test]
fn det_constructor_has_factory_param() {
    let project = Project::load(&fixture("det_project.yaml")).unwrap();
    let backend = EvmSolidityBackend {
        deterministic_addresses: true,
    };
    let files = backend.gen_project(&project);
    let (_, code) = &files[0];
    // Counter constructor should take factory
    assert!(
        code.contains("constructor(address factory_)"),
        "Counter constructor should accept factory param"
    );
    // Vault constructor should take factory and identity
    assert!(
        code.contains("constructor(address factory_, uint64 m_id_)"),
        "Vault constructor should accept factory and identity params, got:\n{}",
        code
    );
}

#[test]
fn det_factory_uses_create2() {
    let project = Project::load(&fixture("det_project.yaml")).unwrap();
    let backend = EvmSolidityBackend {
        deterministic_addresses: true,
    };
    let files = backend.gen_project(&project);
    let (_, code) = &files[0];
    assert!(
        code.contains("salt: bytes32(0)"),
        "factory should use CREATE2 with salt"
    );
    assert!(
        code.contains("0xff"),
        "factory predict should use CREATE2 formula"
    );
}

#[test]
#[allow(non_snake_case)]
fn validation_rejects_det_false_for_evm_with_F6() {
    let dir = tempdir();
    let yaml_path = dir.join("det_false.yaml");
    std::fs::write(
        &yaml_path,
        "name: test-dapp\ntarget: evm\ndeterministic_addresses: false\noutput_dir: build/\nsources:\n  - proj_counter.cam\n",
    )
    .unwrap();
    std::fs::copy(fixture("proj_counter.cam"), dir.join("proj_counter.cam")).unwrap();
    let project = Project::load(&yaml_path).unwrap();
    let diags = cambrian_transpiler::validate::validate_project_config(&project.config);
    assert!(
        diags.iter().any(|d| d.code == "F6"),
        "validator should emit F6 for deterministic_addresses: false on evm, got: {:?}",
        diags.iter().map(|d| (d.code.to_string(), d.message.clone())).collect::<Vec<_>>()
    );
}

#[test]
fn det_implicit_yaml_generates_factory() {
    let dir = tempdir();
    let yaml_path = dir.join("implicit_det.yaml");
    std::fs::write(
        &yaml_path,
        "name: test-dapp\ntarget: evm\noutput_dir: build/\nsources:\n  - proj_counter.cam\n  - proj_vault.cam\n",
    )
    .unwrap();
    std::fs::copy(fixture("proj_counter.cam"), dir.join("proj_counter.cam")).unwrap();
    std::fs::copy(fixture("proj_vault.cam"), dir.join("proj_vault.cam")).unwrap();
    let project = Project::load(&yaml_path).unwrap();
    let backend = EvmSolidityBackend {
        deterministic_addresses: true,
    };
    let files = backend.gen_project(&project);
    let (_, code) = &files[0];
    assert!(
        code.contains("CambrianFactory"),
        "implicit evm yaml must resolve det=true and emit factory"
    );
}

// ===========================================================================
// Deterministic: messaging, from-clause, deploy via factory
// ===========================================================================

/// Helper: load the messaging project and generate code.
fn det_messaging_code() -> String {
    let project = Project::load(&fixture("det_messaging.yaml")).unwrap();
    let backend = EvmSolidityBackend {
        deterministic_addresses: true,
    };
    let files = backend.gen_project(&project);
    files[0].1.clone()
}

/// Helper: extract a single contract's source from the full output.
fn extract_contract<'a>(full: &'a str, name: &str) -> &'a str {
    let marker = format!("contract {} ", name);
    let start = full
        .find(&marker)
        .unwrap_or_else(|| panic!("contract {} not found", name));
    let rest = &full[start..];
    // Find next contract or end
    let end = rest[1..]
        .find("\ncontract ")
        .map(|i| i + 1)
        .unwrap_or(rest.len());
    &rest[..end]
}

#[test]
fn det_send_to_entity_address_generates_create2() {
    let code = det_messaging_code();
    let guardian = extract_contract(&code, "Guardian");
    // Guardian.ping sends `acceptPing() ~> Locker.address(locker_id)`
    // In deterministic mode this lowers via factory.predict* (avoids
    // circular type(E).creationCode references between mutual peers).
    assert!(
        guardian.contains("ICambrianFactory(_factory).predictLocker("),
        "Guardian ping should predict Locker via factory, got:\n{}",
        guardian
    );
}

#[test]
fn det_from_clause_generates_create2_check() {
    let code = det_messaging_code();
    let locker = extract_contract(&code, "Locker");
    // Locker.acceptPing has `from Guardian(m_id)`
    // In deterministic mode this should check msg.sender against predictGuardian.
    assert!(
        locker.contains("msg.sender"),
        "Locker acceptPing should check msg.sender"
    );
    assert!(
        locker.contains("ICambrianFactory(_factory).predictGuardian("),
        "Locker from-clause should predict Guardian via factory, got:\n{}",
        locker
    );
}

#[test]
fn det_deploy_goes_through_factory() {
    let code = det_messaging_code();
    let guardian = extract_contract(&code, "Guardian");
    // Guardian.spawnLocker has `deploy Locker(locker_id)`
    // In deterministic mode this should call ICambrianFactory(_factory).deployLocker(...)
    assert!(
        guardian.contains("ICambrianFactory(_factory).deployLocker("),
        "Guardian spawnLocker should deploy via factory, got:\n{}",
        guardian
    );
    assert!(
        !guardian.contains("new Locker("),
        "Guardian spawnLocker should NOT use 'new Locker' directly"
    );
}

#[test]
fn det_factory_has_deploy_for_messaging_entities() {
    let code = det_messaging_code();
    let factory = extract_contract(&code, "CambrianFactory");
    assert!(
        factory.contains("function deployGuardian("),
        "Factory should have deployGuardian"
    );
    assert!(
        factory.contains("function deployLocker("),
        "Factory should have deployLocker"
    );
    assert!(
        factory.contains("function predictGuardian("),
        "Factory should have predictGuardian"
    );
    assert!(
        factory.contains("function predictLocker("),
        "Factory should have predictLocker"
    );
}

#[test]
fn det_entity_interface_generated_for_cross_entity_calls() {
    let code = det_messaging_code();
    // Guardian sends to Locker, so ILocker interface should be generated
    assert!(
        code.contains("interface ILocker"),
        "ILocker interface should be generated for cross-entity sends"
    );
}

#[test]
fn det_send_uses_interface_for_typed_address() {
    let code = det_messaging_code();
    let guardian = extract_contract(&code, "Guardian");
    // The send `acceptPing() ~> Locker.address(locker_id)` should use ILocker interface
    assert!(
        guardian.contains("ILocker("),
        "Guardian should send via ILocker interface, got:\n{}",
        guardian
    );
    assert!(
        guardian.contains(".acceptPing("),
        "Guardian should call acceptPing method"
    );
}

// ===========================================================================
// Gap 1: addressOf(Entity.state(args)) byte-equivalence with Entity.address(args)
// ===========================================================================

fn det_addressof_code() -> String {
    let project = Project::load(&fixture("det_addressof.yaml")).unwrap();
    let backend = EvmSolidityBackend {
        deterministic_addresses: true,
    };
    let files = backend.gen_project(&project);
    files[0].1.clone()
}

/// Both spellings (`Entity.address(args)` and `addressOf(Entity.state(args))`)
/// must produce the **same** CREATE2 expression in the generated Solidity.
#[test]
fn det_gap1_addressof_byte_equivalent_to_dot_address() {
    let code = det_addressof_code();
    let pinger = extract_contract(&code, "AddrOfPinger");

    // Locate the body of pingDotForm and pingAddressOf
    let dot_start = pinger
        .find("function pingDotForm(")
        .expect("pingDotForm route");
    let dot_end = pinger[dot_start..]
        .find("\n    }\n")
        .map(|i| dot_start + i)
        .unwrap_or(pinger.len());
    let dot_body = &pinger[dot_start..dot_end];

    let aof_start = pinger
        .find("function pingAddressOf(")
        .expect("pingAddressOf route");
    let aof_end = pinger[aof_start..]
        .find("\n    }\n")
        .map(|i| aof_start + i)
        .unwrap_or(pinger.len());
    let aof_body = &pinger[aof_start..aof_end];

    // Extract the CREATE2 sub-expression from each body.
    let create2 = "ICambrianFactory(_factory).predictAddrOfTarget(target_id)";

    assert!(
        dot_body.contains(create2),
        "pingDotForm should contain the CREATE2 expression for AddrOfTarget; got:\n{}",
        dot_body
    );
    assert!(
        aof_body.contains(create2),
        "pingAddressOf should contain the SAME CREATE2 expression as pingDotForm; got:\n{}",
        aof_body
    );
}

/// Both spellings must wrap the call in the same typed interface (IAddrOfTarget).
#[test]
fn det_gap1_addressof_wraps_in_interface() {
    let code = det_addressof_code();
    assert!(
        code.contains("interface IAddrOfTarget"),
        "interface IAddrOfTarget should be auto-collected from both spellings"
    );
    let pinger = extract_contract(&code, "AddrOfPinger");
    let count = pinger.matches("IAddrOfTarget(").count();
    assert!(count >= 2,
        "Both pingDotForm and pingAddressOf should call via IAddrOfTarget(...); got {} occurrences in:\n{}",
        count, pinger);
}

/// Cross-target portability: the `addressOf(Entity.state(args))` spelling
/// must parse identically regardless of target — both EVM and Acki Nacki
/// share the same grammar, so the same source file is parseable for either
/// backend selection.
#[test]
fn det_gap1_addressof_parses_cross_target() {
    let src = std::fs::read_to_string(fixture("det_addressof.cam")).unwrap();
    let parsed = cambrian_transpiler::ProgramParser::new().parse(&src);
    assert!(
        parsed.is_ok(),
        "addressOf(Entity.state(args)) must parse cleanly: {:?}",
        parsed.err()
    );
}

// ===========================================================================
// Gap 5: Entity.address(...) / addressOf(Entity.state(...)) inside member transforms
// ===========================================================================

fn det_member_init_code() -> String {
    let project = Project::load(&fixture("det_member_init.yaml")).unwrap();
    let backend = EvmSolidityBackend {
        deterministic_addresses: true,
    };
    let files = backend.gen_project(&project);
    files[0].1.clone()
}

/// A member transform tagged `in constructor()` that produces a CREATE2
/// address must lower in the constructor body (or initialize() if non-id init
/// params are present). Both spellings should produce the same expression.
#[test]
fn det_gap5_member_init_lowers_create2_in_constructor() {
    let code = det_member_init_code();
    let owner = extract_contract(&code, "Owner");

    let constructor_start = owner.find("constructor(").expect("constructor");
    let constructor_end = owner[constructor_start..]
        .find("\n    }\n")
        .map(|i| constructor_start + i)
        .unwrap_or(owner.len());
    let body = &owner[constructor_start..constructor_end];

    let create2_for_other = "ICambrianFactory(_factory).predictOtherEntity(m_id)";

    assert!(
        body.contains(create2_for_other),
        "constructor should compute CREATE2(OtherEntity, m_id); got:\n{}",
        body
    );
    assert!(
        body.contains("m_other_dot = "),
        "constructor should assign m_other_dot; got:\n{}",
        body
    );
    assert!(
        body.contains("m_other_aof = "),
        "constructor should assign m_other_aof; got:\n{}",
        body
    );
}

/// The two spellings must produce the SAME CREATE2 sub-expression.
#[test]
fn det_gap5_both_spellings_produce_identical_create2() {
    let code = det_member_init_code();
    let owner = extract_contract(&code, "Owner");

    let needle = "next_m_other_dot = ";
    let dot_start = owner.find(needle).expect("next_m_other_dot assignment");
    let dot_end = owner[dot_start..].find(';').map(|i| dot_start + i).unwrap();
    let dot_rhs = &owner[dot_start + needle.len()..dot_end];

    let needle = "next_m_other_aof = ";
    let aof_start = owner.find(needle).expect("next_m_other_aof assignment");
    let aof_end = owner[aof_start..].find(';').map(|i| aof_start + i).unwrap();
    let aof_rhs = &owner[aof_start + needle.len()..aof_end];

    assert_eq!(
        dot_rhs, aof_rhs,
        "Entity.address(args) and addressOf(Entity.state(args)) must produce \
         byte-identical CREATE2 expressions inside member-init transforms"
    );
}

// ===========================================================================
// Gap 6: zero-identity entities (singletons) — factory + from-clause + send
// ===========================================================================

fn det_singleton_code() -> String {
    let project = Project::load(&fixture("det_singleton.yaml")).unwrap();
    let backend = EvmSolidityBackend {
        deterministic_addresses: true,
    };
    let files = backend.gen_project(&project);
    files[0].1.clone()
}

#[test]
fn det_gap6_singleton_factory_predict_takes_no_args() {
    let code = det_singleton_code();
    let factory = extract_contract(&code, "CambrianFactory");
    assert!(
        factory.contains("function predictGlobalRegistry() external view returns (address)"),
        "Singleton predictGlobalRegistry should take no identity args; got:\n{}",
        factory
    );
    assert!(
        factory.contains("function deployGlobalRegistry() external payable returns (address)"),
        "Singleton deployGlobalRegistry should take no identity args; got:\n{}",
        factory
    );
}

#[test]
fn det_gap6_singleton_predict_uses_zero_arg_create2() {
    let code = det_singleton_code();
    let factory = extract_contract(&code, "CambrianFactory");
    // For zero identity args, the CREATE2 inner abi.encode call should only
    // contain the factory address (no identity args).
    assert!(factory.contains(
        "keccak256(abi.encodePacked(type(GlobalRegistry).creationCode, abi.encode(address(this))))"),
        "Singleton predict should encode only factory address (no identity args); got:\n{}", factory);
}

#[test]
fn det_gap6_singleton_send_to_address_works() {
    let code = det_singleton_code();
    let caller = extract_contract(&code, "SomeCaller");
    // `~> GlobalRegistry.address()` (zero-arg) should produce a CREATE2 expr
    // wrapped in IGlobalRegistry(...).ping(m_id).
    assert!(
        caller.contains("IGlobalRegistry("),
        "SomeCaller.invoke should call via IGlobalRegistry interface"
    );
    assert!(
        caller.contains("ICambrianFactory(_factory).predictGlobalRegistry()"),
        "SomeCaller.invoke send should predict GlobalRegistry via factory; got:\n{}",
        caller
    );
}

#[test]
fn det_gap6_singleton_from_clause_emits_zero_arg_create2_check() {
    let code = det_singleton_code();
    let caller = extract_contract(&code, "SomeCaller");
    // `from GlobalRegistry()` (zero args) should emit a require check against
    // the zero-arg CREATE2 expression.
    assert!(
        caller.contains("require((msg.sender =="),
        "SomeCaller.notify should have a from-clause msg.sender check"
    );
    assert!(
        caller.contains("ICambrianFactory(_factory).predictGlobalRegistry()"),
        "SomeCaller.notify from-clause should reference singleton CREATE2; got:\n{}",
        caller
    );
}

// ===========================================================================
// Gap 7: named send to dynamic CREATE2 expression (not a stored Address<E>)
// ===========================================================================

/// `Action(args) ~> Other.address(other_id)` (the dest is a CREATE2 expression
/// computed from a route param, NOT a stored `Address<Other>` member) must
/// lower to `IOther(<CREATE2 expr>).Action(args)` with the args correctly
/// forwarded.
#[test]
fn det_gap7_named_send_to_create2_expr_dot_form() {
    let code = det_addressof_code();
    let pinger = extract_contract(&code, "AddrOfPinger");
    let dot_start = pinger
        .find("function pingDotForm(")
        .expect("pingDotForm route");
    let dot_end = pinger[dot_start..]
        .find("\n    }\n")
        .map(|i| dot_start + i)
        .unwrap_or(pinger.len());
    let body = &pinger[dot_start..dot_end];

    assert!(
        body.contains("IAddrOfTarget("),
        "send to Entity.address(arg) should wrap in IEntity interface; got:\n{}",
        body
    );
    assert!(
        body.contains(").acceptPing();"),
        "method name should be preserved on the typed-interface call; got:\n{}",
        body
    );
    assert!(
        body.contains("target_id"),
        "the route param must flow into the CREATE2 abi.encode; got:\n{}",
        body
    );
}

#[test]
fn det_gap7_named_send_to_create2_expr_addressof_form() {
    let code = det_addressof_code();
    let pinger = extract_contract(&code, "AddrOfPinger");
    let aof_start = pinger
        .find("function pingAddressOf(")
        .expect("pingAddressOf route");
    let aof_end = pinger[aof_start..]
        .find("\n    }\n")
        .map(|i| aof_start + i)
        .unwrap_or(pinger.len());
    let body = &pinger[aof_start..aof_end];

    assert!(
        body.contains("IAddrOfTarget("),
        "send to addressOf(Entity.state(arg)) should wrap in IEntity interface; got:\n{}",
        body
    );
    assert!(
        body.contains(").acceptPing();"),
        "method name should be preserved on the typed-interface call; got:\n{}",
        body
    );
}

// ===========================================================================
// Gap 8: hardening — initialize() must reject non-factory callers
// ===========================================================================

#[test]
fn det_gap8_initialize_guards_only_factory() {
    let project = Project::load(&fixture("det_project.yaml")).unwrap();
    let backend = EvmSolidityBackend {
        deterministic_addresses: true,
    };
    let files = backend.gen_project(&project);
    let (_, code) = &files[0];

    let vault = extract_contract(code, "Vault");
    assert!(
        vault.contains("function initialize("),
        "Vault should have initialize() (it has non-identity init params)"
    );
    let init_start = vault.find("function initialize(").unwrap();
    let init_end = vault[init_start..]
        .find("\n    }\n")
        .map(|i| init_start + i)
        .unwrap_or(vault.len());
    let init_body = &vault[init_start..init_end];

    assert!(
        init_body.contains("require(msg.sender == _factory, \"only factory\")"),
        "initialize() must reject non-factory callers; got:\n{}",
        init_body
    );

    // The only-factory check must come BEFORE the _initialized guard so that
    // a malicious early-caller cannot set the _initialized flag and lock the
    // factory out (defence in depth).
    let only_factory = init_body
        .find("require(msg.sender == _factory")
        .expect("only-factory check");
    let initialised_guard = init_body
        .find("require(!_initialized")
        .expect("_initialized guard");
    assert!(
        only_factory < initialised_guard,
        "only-factory check must come before _initialized guard"
    );
}

// ===========================================================================
// Phases Q1: SSTORE-before-CALL guarantee for unphased routes on EVM
// ===========================================================================

/// In an UNPHASED route, the codegen MUST emit member-update SSTOREs before
/// any external call. This codifies the checks-effects-interactions guarantee
/// that allows authors to write idiomatic unphased Cambrian and still rely on
/// reentrancy safety on EVM.
///
/// If a future codegen refactor reorders these (e.g., moves transforms after
/// actions), this test catches it.
#[test]
fn phases_q1_unphased_route_sstores_before_calls() {
    let project = Project::load(&fixture("det_reentrancy_order.yaml")).unwrap();
    let backend = EvmSolidityBackend {
        deterministic_addresses: true,
    };
    let files = backend.gen_project(&project);
    let (_, code) = &files[0];

    let vault = extract_contract(code, "ReentrancyVault");
    let withdraw_start = vault.find("function withdraw(").expect("withdraw route");
    let withdraw_end = vault[withdraw_start..]
        .find("\n    }\n")
        .map(|i| withdraw_start + i)
        .unwrap_or(vault.len());
    let body = &vault[withdraw_start..withdraw_end];

    let sstore_pos = body
        .find("m_state = next_m_state;")
        .expect("withdraw should write m_state");
    let call_pos = body
        .find(".receivePayload(")
        .expect("withdraw should call receivePayload on the target");

    assert!(
        sstore_pos < call_pos,
        "SSTORE for m_state must appear BEFORE the external CALL to receivePayload \
         (checks-effects-interactions). Body:\n{}",
        body
    );
}

// ===========================================================================
// Phases Q2A: per-phase where clause codegen on EVM
// ===========================================================================

/// A per-phase `where` clause must lower to a `require(...)` placed at the
/// very top of the corresponding phase block, AFTER any vars defined in
/// strictly earlier phases are bound and BEFORE this phase's transforms or
/// actions run. The label embedded in the failure string carries both the
/// phase name and the user's error code, so a runtime revert points back to
/// the right source location.
#[test]
fn phase_where_lowers_to_require_in_phase_block() {
    let project = Project::load(&fixture("phase_where_var.yaml")).unwrap();
    let backend = EvmSolidityBackend {
        deterministic_addresses: true,
    };
    let files = backend.gen_project(&project);
    let (_, code) = &files[0];

    let vault = extract_contract(code, "Vault");
    let withdraw_start = vault.find("function withdraw(").expect("withdraw route");
    let withdraw_end = vault[withdraw_start..]
        .find("\n    }\n")
        .map(|i| withdraw_start + i)
        .unwrap_or(vault.len());
    let body = &vault[withdraw_start..withdraw_end];

    let fetch_pos = body.find("// Phase: fetch").expect("fetch phase comment");
    let act_pos = body.find("// Phase: act").expect("act phase comment");
    // Per-phase `where : throw N` lowers to `require(..., "throw(N)")` so
    // that runtime test harnesses (`expect throw N`) can match the revert
    // string against the user-supplied numeric code, not an internal label.
    let expected_require = "require((bal > 0), \"throw(401)\")";
    let require_pos = body
        .find(expected_require)
        .unwrap_or_else(|| panic!("expected {} in withdraw body:\n{}", expected_require, body));

    assert!(
        fetch_pos < act_pos,
        "Phase fetch must come before phase act in generated body"
    );
    assert!(
        act_pos < require_pos,
        "Per-phase where require(...) must be emitted INSIDE the act phase \
         block, not before it. Body:\n{}",
        body
    );

    let act_body = &body[act_pos..];
    let require_in_act = act_body
        .find(expected_require)
        .expect("require should appear inside act block");
    let next_phase_or_end = act_body[require_in_act..]
        .find("// Phase:")
        .map(|i| require_in_act + i)
        .unwrap_or(act_body.len());
    assert!(
        next_phase_or_end > require_in_act,
        "require(...) must be the first statement in the act phase, before any \
         transform or action of that phase. Body:\n{}",
        body
    );
}

// ===========================================================================
// program_with_entity_first helper
// ===========================================================================

#[test]
fn program_with_entity_first_reorders() {
    let project = Project::load(&fixture("project.yaml")).unwrap();
    let reordered =
        cambrian_transpiler::project::program_with_entity_first(&project.merged, "Vault");
    assert_eq!(reordered.entities[0].name, "Vault");
    assert_eq!(reordered.entities.len(), 2);
}

// ===========================================================================
// revm test backend (config + generated crate layout)
// ===========================================================================

#[test]
fn revm_tests_disabled_by_default() {
    let project = Project::load(&fixture("project.yaml")).unwrap();
    assert!(project.config.revm_tests.is_none());
    let backend = EvmSolidityBackend {
        deterministic_addresses: true,
    };
    let files = backend.gen_project(&project);
    assert!(
        !files.iter().any(|(p, _)| p.starts_with("revm-tests/")),
        "revm-tests/ should NOT be generated unless the project YAML opts in"
    );
}

// ===========================================================================
// Fuzz / property testing fixtures
// ===========================================================================

#[test]
fn project_fuzz_yaml_loads_and_resolves_defaults() {
    let project = Project::load(&fixture("project_fuzz.yaml")).unwrap();
    let cfg = project.config.fuzz.as_ref().expect("fuzz block parsed");
    assert_eq!(cfg.runs, 32);
    assert_eq!(cfg.seed, 1);
    assert!(cfg.shrink);
    assert_eq!(cfg.max_local_rejects, 1024);

    let resolved = project.config.resolved_fuzz();
    assert_eq!(resolved.runs, 32);
    assert_eq!(resolved.max_local_rejects, 1024);

    assert!(
        !project.merged.fuzz_tests.is_empty(),
        "merged program should contain fuzz declarations from counter.fuzz.cam"
    );
}

#[test]
fn foundry_codegen_emits_test_fuzz_helpers() {
    let project = Project::load(&fixture("project_fuzz.yaml")).unwrap();
    let backend = EvmSolidityBackend {
        deterministic_addresses: true,
    };
    let files = backend.gen_project(&project);

    let (_, sol) = files
        .iter()
        .find(|(p, _)| p == "test/Counter.t.sol")
        .expect("Foundry test file must be generated when fuzz tests are present");

    assert!(
        sol.contains("function testFuzz_"),
        "Foundry codegen should emit testFuzz_<name>(...) functions"
    );
    assert!(
        sol.contains("vm.assume("),
        "Foundry codegen should translate `assume` into vm.assume()"
    );
    assert!(
        sol.contains("bound("),
        "Foundry codegen should translate `bound` into Foundry's StdUtils.bound()"
    );

    let (_, toml) = files
        .iter()
        .find(|(p, _)| p == "foundry.toml")
        .expect("foundry.toml must exist");
    assert!(
        toml.contains("[profile.default.fuzz]"),
        "foundry.toml should declare a [profile.default.fuzz] section"
    );
    assert!(
        toml.contains("runs = 32"),
        "foundry.toml should pick up runs from project.fuzz.runs (got: {})",
        toml
    );
}

// ===========================================================================
// Stateful invariant fixtures
// ===========================================================================

#[test]
fn invariant_yaml_loads_and_resolves_defaults() {
    let project = Project::load(&fixture("project_invariant.yaml")).unwrap();
    let cfg = project
        .config
        .invariant
        .as_ref()
        .expect("invariant block parsed");
    assert_eq!(cfg.runs, 16);
    assert_eq!(cfg.depth, 8);
    assert!(!cfg.fail_on_revert);
    assert_eq!(cfg.seed, 1);
    assert_eq!(cfg.max_local_rejects, 1024);

    let resolved = project.config.resolved_invariant();
    assert_eq!(resolved.runs, 16);
    assert_eq!(resolved.depth, 8);

    assert!(
        !project.merged.invariants.is_empty(),
        "merged program should contain invariant declarations from counter.invariant.cam"
    );
    assert_eq!(project.merged.invariants.len(), 4);
}

/// Forall-ized invariant initial state (`init { m_count: * }`) seeds a
/// fuzzer-random starting value on the dynamic Foundry / revm targets
/// (rather than being Lean-only).
#[test]
fn dynamic_forall_invariant_init_seeds_random_state() {
    let project = Project::load(&fixture("project_invariant.yaml")).unwrap();
    let backend = EvmSolidityBackend {
        deterministic_addresses: true,
    };
    let files = backend.gen_project(&project);

    // Foundry: setUp() declares a vm.random* local and vm.stores it.
    let (_, sol) = files
        .iter()
        .find(|(p, _)| p == "test/Invariant_Counter_count_bounded_from_any_start.t.sol")
        .expect("forall invariant Foundry test file must be generated");
    assert!(
        sol.contains("uint256 __fa_m_count = vm.randomUint();"),
        "forall field should be seeded from a vm.random* cheatcode:\n{sol}"
    );
    assert!(
        sol.contains(
            "vm.store(address(_counter), bytes32(uint256(0)), bytes32(uint256(__fa_m_count)));"
        ),
        "the random forall value should be written into the SUT storage slot:\n{sol}"
    );

    // revm: the forall field becomes an extra proptest input written before
    // the action sequence runs.
    #[cfg(feature = "revm")]
    {
        let (_, rs) = files
            .iter()
            .find(|(p, _)| p == "revm-tests/tests/counter.rs")
            .expect("revm-tests/tests/counter.rs must exist");
        assert!(
            rs.contains("__fa_m_count in any::<[u8; 32]>().prop_map(|b| U256::from_be_bytes(b))"),
            "forall field should be sampled as a proptest input:\n{rs}"
        );
        assert!(
            rs.contains("t.write_storage(U256::from(0u64), __fa_m_count);"),
            "the sampled forall value should be written into storage before the trace:\n{rs}"
        );
    }
}

/// A forall-ized `ctx { ... }` block on an invariant seeds the blockchain
/// context dynamically: `sys::now: *` randomizes the initial timestamp and
/// `msg::sender: *` varies the caller. Foundry randomizes the timestamp in
/// `setUp` and leaves the sender pool unrestricted; revm samples both as
/// proptest inputs and applies them before the trace.
#[test]
fn dynamic_forall_invariant_ctx_seeds_random_context() {
    let project = Project::load(&fixture("project_invariant.yaml")).unwrap();
    let backend = EvmSolidityBackend {
        deterministic_addresses: true,
    };
    let files = backend.gen_project(&project);

    // Foundry: setUp() warps to a random timestamp and leaves senders open.
    let (_, sol) = files
        .iter()
        .find(|(p, _)| p == "test/Invariant_Counter_count_bounded_from_any_time.t.sol")
        .expect("ctx-forall invariant Foundry test file must be generated");
    assert!(
        sol.contains("vm.warp(vm.randomUint());"),
        "forall sys::now should randomize the initial timestamp:\n{sol}"
    );
    assert!(
        sol.contains("senders left unrestricted"),
        "forall msg::sender should leave Foundry's sender pool unrestricted:\n{sol}"
    );

    // revm: both ctx params become proptest inputs applied before the trace.
    #[cfg(feature = "revm")]
    {
        let (_, rs) = files
            .iter()
            .find(|(p, _)| p == "revm-tests/tests/counter.rs")
            .expect("revm-tests/tests/counter.rs must exist");
        assert!(
            rs.contains("__ctx_sys_now in any::<u64>()"),
            "forall sys::now should be sampled as a proptest input:\n{rs}"
        );
        assert!(
            rs.contains("t.set_timestamp(__ctx_sys_now);"),
            "the sampled timestamp should be applied before the trace:\n{rs}"
        );
        assert!(
            rs.contains("__ctx_msg_sender in any::<[u8; 20]>().prop_map(Address::from)"),
            "forall msg::sender should be sampled as a proptest input:\n{rs}"
        );
        assert!(
            rs.contains("t.set_sender(__ctx_msg_sender);"),
            "the sampled sender should be applied before the trace:\n{rs}"
        );
    }
}

/// Forall-ized state members are sampled according to their declared type so
/// the seeded storage slot is canonical: `address` masks to 160 bits and
/// `bool` collapses to `0/1`, while wide integers keep a full 32-byte sample.
/// (A raw 32-byte write leaves the slot "dirty", which Solidity bool reads in
/// particular mishandle.)
#[test]
fn dynamic_forall_invariant_state_sampling_is_type_aware() {
    let src = r#"
entity Registry {
    routes { touch() => [] }
    m_owner: address { in touch() => m_owner }
    m_active: bool { in touch() => m_active }
    m_big: U256 { in touch() => m_big }
}

invariant "fields stay" for Registry {
    init { * }
    action touch() { }
    check m_big >= 0
}
"#;
    let yaml = "target: evm\nsources:\n  - reg.cam\ninvariant:\n  runs: 8\n  depth: 4\nrevm_tests:\n  enabled: true\n";
    let dir = tempdir();
    std::fs::write(dir.join("reg.cam"), src).unwrap();
    let yaml_path = dir.join("project.yaml");
    std::fs::write(&yaml_path, yaml).unwrap();

    let project = Project::load(&yaml_path).unwrap();
    let backend = EvmSolidityBackend {
        deterministic_addresses: true,
    };
    let files = backend.gen_project(&project);

    // revm: address → clean 20-byte, bool → 0/1, U256 → full 32-byte.
    #[cfg(feature = "revm")]
    {
        let (_, rs) = files
            .iter()
            .find(|(p, _)| p == "revm-tests/tests/registry.rs")
            .expect("revm-tests/tests/registry.rs must exist");
        assert!(
            rs.contains("__fa_m_owner in any::<[u8; 20]>().prop_map(|b| U256::from_be_slice(&b))"),
            "address forall should sample a clean 160-bit value:\n{rs}"
        );
        assert!(rs.contains("__fa_m_active in any::<bool>().prop_map(|b| if b { U256::from(1u64) } else { U256::ZERO })"),
        "bool forall should sample a clean 0/1 value:\n{rs}");
        assert!(
            rs.contains("__fa_m_big in any::<[u8; 32]>().prop_map(|b| U256::from_be_bytes(b))"),
            "wide-int forall should keep a full 32-byte sample:\n{rs}"
        );
    }

    // Foundry: address → uint160-masked, bool → % 2, U256 → raw.
    let (_, sol) = files
        .iter()
        .find(|(p, _)| p == "test/Invariant_Registry_fields_stay.t.sol")
        .expect("Foundry invariant test file must exist");
    assert!(
        sol.contains("uint256 __fa_m_owner = uint256(uint160(vm.randomUint()));"),
        "address forall should mask to 160 bits on Foundry:\n{sol}"
    );
    assert!(
        sol.contains("uint256 __fa_m_active = vm.randomUint() % 2;"),
        "bool forall should collapse to 0/1 on Foundry:\n{sol}"
    );
    assert!(
        sol.contains("uint256 __fa_m_big = vm.randomUint();"),
        "wide-int forall should keep a raw sample on Foundry:\n{sol}"
    );
}

/// "Randomize everything except this field": a bare `*` plus an explicit pin
/// (`init { *, m_total_assets: 0 }`) seeds every non-pinned state member with
/// a fuzzer-random value while holding the pinned member fixed.
#[test]
fn dynamic_forall_invariant_all_but_pinned_field() {
    let project = Project::load(&fixture("project_invariant_lending.yaml")).unwrap();
    let backend = EvmSolidityBackend {
        deterministic_addresses: true,
    };
    let files = backend.gen_project(&project);

    let (_, sol) = files
        .iter()
        .find(|(p, _)| {
            p == "test/Invariant_LendingPair_borrowed_bounded_from_any_start_but_assets.t.sol"
        })
        .expect("all-but-pinned forall invariant Foundry test file must be generated");

    // The non-pinned member is seeded from a random cheatcode...
    assert!(
        sol.contains("uint256 __fa_m_borrowed = vm.randomUint();"),
        "the non-pinned member m_borrowed should be randomized:\n{sol}"
    );
    // Packed `u128` neighbors share a slot, so the write may be a full-word
    // `vm.store` or a read-modify-write; either form must mention the random
    // local.
    assert!(
        sol.contains("bytes32(uint256(__fa_m_borrowed))")
            || sol.contains("uint256(__fa_m_borrowed)"),
        "the random value should be stored into m_borrowed's slot:\n{sol}"
    );
    // ...while the pinned member keeps its literal and is NOT randomized.
    assert!(
        !sol.contains("__fa_m_total_assets"),
        "the pinned member m_total_assets must not be randomized:\n{sol}"
    );
}

#[test]
fn foundry_codegen_emits_handler_and_invariant_test() {
    let project = Project::load(&fixture("project_invariant.yaml")).unwrap();
    let backend = EvmSolidityBackend {
        deterministic_addresses: true,
    };
    let files = backend.gen_project(&project);

    let (_, sol) = files
        .iter()
        .find(|(p, _)| p == "test/Invariant_Counter_count_never_exceeds_bound.t.sol")
        .expect("invariant test file must be generated");

    assert!(
        sol.contains("contract CounterHandler_count_never_exceeds_bound"),
        "should declare per-invariant Handler contract"
    );
    assert!(
        sol.contains("targetContract(address(_handler))"),
        "should register the handler as the invariant target"
    );
    assert!(
        sol.contains("targetSelector(FuzzSelector"),
        "should restrict invariant calls to handler selectors"
    );
    assert!(
        sol.contains("function invariant_count_never_exceeds_bound("),
        "should emit one invariant_<name> function per check clause"
    );

    let (_, toml) = files
        .iter()
        .find(|(p, _)| p == "foundry.toml")
        .expect("foundry.toml must exist");
    assert!(
        toml.contains("[profile.default.invariant]"),
        "foundry.toml should declare a [profile.default.invariant] section"
    );
    assert!(
        toml.contains("runs = 16"),
        "foundry.toml should pick up runs from project.invariant.runs (got: {})",
        toml
    );
    assert!(
        toml.contains("depth = 8"),
        "foundry.toml should pick up depth from project.invariant.depth (got: {})",
        toml
    );
}

/// Trace-aware invariant conditions (`trace::length` / `count` /
/// `lastWas`) wire per-run accumulator counters into both dynamic EVM
/// backends, and `assume` conditions go through the same state-getter
/// substitution as `check`.
#[test]
fn dynamic_trace_invariant_emits_counters_on_foundry_and_revm() {
    let project = Project::load(&fixture("project_trace_invariant.yaml")).unwrap();
    let backend = EvmSolidityBackend {
        deterministic_addresses: true,
    };
    let files = backend.gen_project(&project);

    // --- Foundry: handler storage counters + vm.assume + bumps. ---
    let (_, sol) = files
        .iter()
        .find(|(p, _)| p == "test/Invariant_Counter_trace_bounded_counter.t.sol")
        .expect("trace invariant Foundry test file must be generated");
    assert!(
        sol.contains("uint256 public _traceLen;"),
        "Foundry should declare a _traceLen counter:\n{sol}"
    );
    assert!(
        sol.contains("uint256 public _traceCount_increment;")
            && sol.contains("uint256 public _traceCount_reset;"),
        "Foundry should declare per-route _traceCount counters:\n{sol}"
    );
    assert!(
        sol.contains("bool public _traceLast_reset;"),
        "Foundry should declare _traceLast flags for lastWas:\n{sol}"
    );
    assert!(
        sol.contains("vm.assume((_traceLen < 8));"),
        "Foundry should lower trace::length assume to vm.assume:\n{sol}"
    );
    assert!(
        sol.contains("_traceLen++;"),
        "Foundry should bump _traceLen after a successful action:\n{sol}"
    );
    assert!(
        sol.contains("_traceCount_increment++;"),
        "Foundry should bump the per-route counter:\n{sol}"
    );

    // --- revm: proptest-loop locals + prop_assume! + bumps. ---
    #[cfg(feature = "revm")]
    {
        let (_, rs) = files
            .iter()
            .find(|(p, _)| p == "revm-tests/tests/counter.rs")
            .expect("revm-tests/tests/counter.rs must exist");
        assert!(
            rs.contains("let mut trace_len: U256 = U256::ZERO;"),
            "revm should declare a trace_len local:\n{rs}"
        );
        assert!(
            rs.contains("let mut trace_count_increment: U256 = U256::ZERO;"),
            "revm should declare per-route trace_count locals:\n{rs}"
        );
        assert!(
            rs.contains("let mut trace_last_reset: bool = false;"),
            "revm should declare trace_last flags:\n{rs}"
        );
        assert!(
            rs.contains("prop_assume!((trace_len < U256::from(8u64)));"),
            "revm should lower trace::length assume to prop_assume!:\n{rs}"
        );
        assert!(
            rs.contains("trace_len += U256::from(1u8);"),
            "revm should bump trace_len after a successful call:\n{rs}"
        );
        assert!(
            rs.contains("trace_count_increment += U256::from(1u8);"),
            "revm should bump the per-route counter:\n{rs}"
        );
    }
}

/// End-to-end snapshot for the Foundry
/// pipeline: the lending fixture exercises tagged invariants, computed
/// bounds, snapshot/derived helpers, time advancement, and selector /
/// sender exclusions. Failures here usually mean a regression in one
/// of those features.
#[test]
fn foundry_codegen_lending_invariant_uses_all_features() {
    let project = Project::load(&fixture("project_invariant_lending.yaml")).unwrap();
    let backend = EvmSolidityBackend {
        deterministic_addresses: true,
    };
    let files = backend.gen_project(&project);

    let (_, sol) = files
        .iter()
        .find(|(p, _)| p == "test/Invariant_LendingPair_borrowed_never_exceeds_total_assets.t.sol")
        .expect("invariant test file must be generated");

    // Tag propagation: function-name suffix + revert-message prefix +
    // doc comment.
    assert!(
        sol.contains("function invariant_borrowed_never_exceeds_total_assets_0_INV_LEND_001()"),
        "tag should appear in the generated function name suffix:\n{sol}"
    );
    assert!(
        sol.contains("\"[INV-LEND-001] invariant"),
        "tag should prefix the revert message:\n{sol}"
    );
    assert!(
        sol.contains("/// @dev tag: INV-LEND-001"),
        "tag should appear as a NatSpec comment:\n{sol}"
    );

    // Per-decl runs/depth annotations.
    assert!(
        sol.contains("forge-config: default.invariant.runs = 2000"),
        "runs(2000) attribute should propagate as a forge-config comment:\n{sol}"
    );
    assert!(
        sol.contains("forge-config: default.invariant.depth = 80"),
        "depth(80) attribute should propagate as a forge-config comment:\n{sol}"
    );

    // Track snapshot bindings -> handler state slots + constructor.
    assert!(
        sol.contains("uint256 public initial_total;"),
        "track binding should become a public state field:\n{sol}"
    );
    assert!(
        sol.contains("initial_total = _lendingPair.m_total_assets();"),
        "track binding should be captured in the handler constructor:\n{sol}"
    );

    // Derived view helper.
    assert!(
        sol.contains("function utilization() public view returns (uint128)"),
        "derived helper should become a view function on the handler:\n{sol}"
    );

    // Computed bound: lo/hi reference SUT members; result is cast to the
    // parameter's declared Solidity type (u128 → uint128), not hard-coded
    // uint256 (T-X-008 / T-G-001).
    assert!(
        sol.contains("uint128(bound(amount, 1, (_lendingPair.m_total_assets()) - 1))"),
        "computed bound should rewrite member access and cast to param type:\n{sol}"
    );

    // skip if -> if (cond) return;
    assert!(
        sol.contains("if ((_lendingPair.m_total_assets() == 0)) return;"),
        "'skip if' should lower to an early-return guard:\n{sol}"
    );

    // try/catch wrapping when fail_on_revert is false (the default).
    assert!(
        sol.contains("try _lendingPair.deposit(amount) {} catch { return; }"),
        "action calls should be wrapped in try/catch when !fail_on_revert:\n{sol}"
    );

    // with_time -> advanceTime synthetic action + selector listing.
    assert!(
        sol.contains("function advanceTime(uint256 secs) public"),
        "with_time should emit a synthetic advanceTime action:\n{sol}"
    );
    assert!(
        sol.contains("bytes4(keccak256(\"advanceTime(uint256)\"))"),
        "advanceTime should be in the targetSelector list:\n{sol}"
    );

    // exclude senders / exclude selectors.
    assert!(
        sol.contains("excludeSender("),
        "exclude senders should produce excludeSender(...) calls:\n{sol}"
    );
    assert!(
        sol.contains("excludeSelector(FuzzSelector"),
        "exclude selectors should produce excludeSelector(...) calls:\n{sol}"
    );
    assert!(
        sol.contains("excluded[0] = bytes4(keccak256(\"accrueInterest()\"));"),
        "named selector should be hashed and excluded:\n{sol}"
    );

    // Profile section in foundry.toml.
    let (_, toml) = files
        .iter()
        .find(|(p, _)| p == "foundry.toml")
        .expect("foundry.toml must exist");
    assert!(
        toml.contains("[profile.cambrian]"),
        "foundry.toml should declare the cheap [profile.cambrian]:\n{toml}"
    );
    assert!(
        toml.contains("[profile.cambrian_night]"),
        "foundry.toml should declare the overnight [profile.cambrian_night]:\n{toml}"
    );
}

// ===========================================================================
// Multi-entity (`for { ... }`) invariants
// ===========================================================================

#[test]
fn multi_entity_invariant_yaml_loads() {
    let project = Project::load(&fixture("project_invariant_multi.yaml")).unwrap();
    assert_eq!(project.merged.invariants.len(), 1);
    let inv = &project.merged.invariants[0];
    assert!(
        !inv.is_single_entity(),
        "fixture should parse as a multi-entity invariant"
    );
    assert_eq!(inv.instances.len(), 3);
    let names: Vec<&str> = inv.instances.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, vec!["v", "a", "t"]);
    let entities: Vec<&str> = inv.instances.iter().map(|i| i.entity.as_str()).collect();
    assert_eq!(entities, vec!["Vault", "Vault", "Treasury"]);
}

#[test]
fn foundry_codegen_emits_multi_handler_and_targets() {
    let project = Project::load(&fixture("project_invariant_multi.yaml")).unwrap();
    let backend = EvmSolidityBackend {
        deterministic_addresses: true,
    };
    let files = backend.gen_project(&project);

    let (_, sol) = files
        .iter()
        .find(|(p, _)| p == "test/Invariant_vault_sum_is_bounded.t.sol")
        .expect("multi-instance invariant test file must be generated");

    assert!(
        sol.contains("contract Handler_vault_sum_is_bounded"),
        "multi-instance Handler should be emitted"
    );
    assert!(
        sol.contains("Vault public _v;")
            && sol.contains("Vault public _a;")
            && sol.contains("Treasury public _t;"),
        "Handler should hold one storage slot per declared instance"
    );
    assert!(
        sol.contains("function v_deposit("),
        "wrapper for v.deposit must be emitted"
    );
    assert!(
        sol.contains("function a_deposit("),
        "wrapper for a.deposit must be emitted"
    );
    assert!(
        sol.contains("function t_credit("),
        "wrapper for t.credit must be emitted"
    );
    assert!(
        sol.contains("targetContract(address(_handler))"),
        "Handler should be the single targetContract"
    );
    assert!(
        sol.contains("new CambrianFactory()"),
        "multi-instance invariant setUp must deploy CambrianFactory (U4-4c)"
    );
    assert!(
        sol.contains("cam_wire("),
        "handler-first setUp must wire instances via cam_wire"
    );
    assert!(
        !sol.contains(".initialize("),
        "factory.deploy* must not be followed by .initialize in invariant setUp"
    );
    assert!(
        sol.contains("_v.m_balance()")
            && sol.contains("_a.m_balance()")
            && sol.contains("_t.m_total()"),
        "checks should resolve <inst>.<member> to _<inst>.<member>() getters"
    );
}

#[test]
fn ackinacki_target_rejects_multi_entity_invariant_with_I7() {
    // I7 is Domains([Tvm]) since the P7 classification review: it fires via
    // check_target_compat on the TVM domain, not in target-less validate.
    let project = Project::load(&fixture("project_invariant_multi.yaml")).unwrap();
    let diags = cambrian_transpiler::validate::check_target_compat(
        &project.merged,
        cambrian_transpiler::target::Target::AckiNacki,
        false,
    );
    let i7: Vec<_> = diags.iter().filter(|d| d.code == "I7").collect();
    assert!(
        !i7.is_empty(),
        "validator should emit I7 for the multi-instance invariant on the Acki Nacki target"
    );
    let evm = cambrian_transpiler::validate::check_target_compat(
        &project.merged,
        cambrian_transpiler::target::Target::Evm,
        false,
    );
    assert!(
        evm.iter().all(|d| d.code != "I7"),
        "I7 must not fire on EVM-domain targets (multi-entity invariants are supported there)"
    );
}

#[test]
fn validation_rejects_coverage_without_revm_tests() {
    // F1: revm_tests.coverage.enabled requires revm_tests.enabled = true.
    let yaml = "\
name: counter-bad-f1
target: evm
output_dir: build_bad_f1/
sources:
  - counter.cam

revm_tests:
  enabled: false
  coverage:
    enabled: true
    engine: libfuzzer
";
    let dir = tempdir();
    let yaml_path = dir.join("project.yaml");
    std::fs::write(&yaml_path, yaml).unwrap();
    std::fs::copy(fixture("counter.cam"), dir.join("counter.cam")).unwrap();

    let project = Project::load(&yaml_path).unwrap();
    let diags = cambrian_transpiler::validate::validate_project_config(&project.config);
    let f1: Vec<_> = diags.iter().filter(|d| d.code == "F1").collect();
    assert!(!f1.is_empty(),
        "validator should emit F1 when revm_tests.coverage.enabled but revm_tests is disabled, got diags: {:?}",
        diags.iter().map(|d| (d.code.to_string(), d.message.to_string())).collect::<Vec<_>>());
}

#[test]
#[allow(non_snake_case)]
fn validation_rejects_unknown_coverage_engine_with_F3() {
    let yaml = "\
name: counter-bad-f3
target: evm
output_dir: build_bad_f3/
sources:
  - counter.cam

revm_tests:
  enabled: true
  coverage:
    enabled: true
    engine: notarealengine
";
    let dir = tempdir();
    let yaml_path = dir.join("project.yaml");
    std::fs::write(&yaml_path, yaml).unwrap();
    std::fs::copy(fixture("counter.cam"), dir.join("counter.cam")).unwrap();

    let project = Project::load(&yaml_path).unwrap();
    let diags = cambrian_transpiler::validate::validate_project_config(&project.config);
    assert!(
        diags.iter().any(|d| d.code == "F3"),
        "validator should emit F3 for unknown coverage engine, got: {:?}",
        diags.iter().map(|d| d.code.to_string()).collect::<Vec<_>>()
    );
}

#[cfg(not(feature = "revm"))]
#[test]
fn revm_tests_enabled_does_not_emit_crate_without_feature() {
    let project = Project::load(&fixture("project_revm.yaml")).unwrap();
    assert!(
        project
            .config
            .revm_tests
            .as_ref()
            .is_some_and(|c| c.enabled),
        "YAML must still parse revm_tests.enabled"
    );
    let backend = EvmSolidityBackend {
        deterministic_addresses: true,
    };
    let files = backend.gen_project(&project);
    assert!(
        !files.iter().any(|(p, _)| p.starts_with("revm-tests/")),
        "revm-tests/ must not be emitted without the revm feature, got: {:#?}",
        files.iter().map(|(p, _)| p).collect::<Vec<_>>()
    );
}

#[cfg(not(feature = "predictable-profile"))]
#[test]
fn lean_predictable_profile_requires_feature() {
    let yaml = "\
name: predictable-off
target: lean
sources:
  - counter.cam
lean:
  emission_profile: predictable
";
    let dir = tempdir();
    std::fs::write(
        dir.join("counter.cam"),
        "entity Counter { routes { ping() => [] } }\n",
    )
    .unwrap();
    let yaml_path = dir.join("project.yaml");
    std::fs::write(&yaml_path, yaml).unwrap();
    let project = Project::load(&yaml_path).unwrap();
    let diags = cambrian_transpiler::validate::validate_project_config(&project.config);
    assert!(
        diags
            .iter()
            .any(|d| d.code == "F2" && d.message.contains("predictable-profile")),
        "predictable profile must be F2 without predictable-profile, got: {:?}",
        diags
            .iter()
            .map(|d| (d.code.to_string(), d.message.to_string()))
            .collect::<Vec<_>>()
    );
}

#[cfg(not(feature = "rust-targets"))]
#[test]
fn yaml_native_target_is_unknown_without_feature() {
    let yaml = "target: native\nsources:\n  - counter.cam\n";
    let dir = tempdir();
    std::fs::write(
        dir.join("counter.cam"),
        "entity Counter { routes { ping() => [] } }\n",
    )
    .unwrap();
    let yaml_path = dir.join("project.yaml");
    std::fs::write(&yaml_path, yaml).unwrap();
    let err = Project::load(&yaml_path).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("unknown target") && msg.contains("evm|lean"),
        "native must be rejected without rust-targets, got: {msg}"
    );
}

// ===========================================================================
// library_paths / imports / F5
// ===========================================================================

const LIB_DOUBLE: &str = "pure fn double(x: u64) -> u64 { x * 2 }\n";
const FOO_USES_DOUBLE: &str = r#"
entity Foo {
    routes {
        set(v: u64) => []
    }
    m_v: u64 {
        in set(v) => double(v)
    }
}
"#;
const FOO_PASSTHROUGH: &str = r#"
entity Foo {
    routes {
        set(v: u64) => []
    }
    m_v: u64 {
        in set(v) => v
    }
}
"#;

fn write_rel(dir: &Path, rel: &str, contents: &str) {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

fn evm_project_sol(project: &Project) -> String {
    let backend = EvmSolidityBackend {
        deterministic_addresses: true,
    };
    let files = backend.gen_project(project);
    files
        .into_iter()
        .find(|(p, _)| p.contains("_project.sol"))
        .map(|(_, s)| s)
        .expect("expected a combined _project.sol")
}

#[test]
fn yaml_imports_resolve_from_library_path() {
    let root = tempdir();
    let proj = root.join("proj");
    let lib = root.join("lib");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::create_dir_all(lib.join("token")).unwrap();
    write_rel(&lib, "token/core.cam", LIB_DOUBLE);
    write_rel(&proj, "Foo.cam", FOO_USES_DOUBLE);
    write_rel(
        &proj,
        "project.yaml",
        "target: evm\n\
         sources:\n\
         \x20\x20- Foo.cam\n\
         library_paths:\n\
         \x20\x20- ../lib\n\
         imports:\n\
         \x20\x20- token/core.cam\n",
    );

    let project = Project::load(&proj.join("project.yaml")).unwrap();
    assert!(
        project.merged.pure_fns.iter().any(|f| f.name == "double"),
        "yaml imports: must pull token/core.cam from library_paths"
    );
    assert!(project.merged.entities.iter().any(|e| e.name == "Foo"));
}

#[test]
fn yaml_imports_miss_is_f5_and_names_probed_roots() {
    let root = tempdir();
    let proj = root.join("proj");
    let lib = root.join("lib");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::create_dir_all(&lib).unwrap();
    write_rel(&proj, "Foo.cam", FOO_PASSTHROUGH);
    write_rel(
        &proj,
        "project.yaml",
        "target: evm\n\
         sources:\n\
         \x20\x20- Foo.cam\n\
         library_paths:\n\
         \x20\x20- ../lib\n\
         imports:\n\
         \x20\x20- missing.cam\n",
    );

    match Project::load(&proj.join("project.yaml")) {
        Err(ProjectError::Packaging { code, message, .. }) => {
            assert_eq!(code, "F5");
            assert!(message.contains("missing.cam"), "{message}");
            assert!(
                message.contains("probed:"),
                "F5 must name probed roots: {message}"
            );
            assert!(message.contains("missing.cam"), "{message}");
        }
        other => panic!("expected F5 Packaging, got {other:?}"),
    }
}

#[test]
fn yaml_imports_entity_is_f4() {
    let dir = tempdir();
    write_rel(&dir, "Foo.cam", FOO_PASSTHROUGH);
    write_rel(
        &dir,
        "lib_entity.cam",
        "entity LibraryEntity {\n\
             routes { noop() => [] }\n\
             m_x: u64 { in noop() => 0 }\n\
         }\n",
    );
    write_rel(
        &dir,
        "project.yaml",
        "target: evm\n\
         sources:\n\
         \x20\x20- Foo.cam\n\
         imports:\n\
         \x20\x20- lib_entity.cam\n",
    );

    match Project::load(&dir.join("project.yaml")) {
        Err(ProjectError::Packaging { code, message, .. }) => {
            assert_eq!(code, "F4");
            assert!(message.contains("LibraryEntity"), "{message}");
        }
        other => panic!("expected F4 Packaging, got {other:?}"),
    }
}

#[test]
fn yaml_sources_win_over_imports_for_same_path() {
    let dir = tempdir();
    write_rel(&dir, "Foo.cam", FOO_PASSTHROUGH);
    write_rel(
        &dir,
        "project.yaml",
        "target: evm\n\
         sources:\n\
         \x20\x20- Foo.cam\n\
         imports:\n\
         \x20\x20- Foo.cam\n",
    );

    let project = Project::load(&dir.join("project.yaml")).unwrap();
    assert_eq!(project.merged.entities.len(), 1);
    assert_eq!(project.merged.entities[0].name, "Foo");
}

#[test]
fn yaml_library_keys_absent_or_empty_match_legacy_evm() {
    let dir = tempdir();
    write_rel(&dir, "math.cam", LIB_DOUBLE);
    write_rel(
        &dir,
        "Foo.cam",
        &(r#"import "./math.cam""#.to_string() + FOO_USES_DOUBLE),
    );
    let legacy = "target: evm\nsources:\n  - Foo.cam\n";
    let empty_keys = "target: evm\n\
         sources:\n\
         \x20\x20- Foo.cam\n\
         library_paths: []\n\
         imports: []\n";
    write_rel(&dir, "legacy.yaml", legacy);
    write_rel(&dir, "empty.yaml", empty_keys);

    let a = Project::load(&dir.join("legacy.yaml")).unwrap();
    let b = Project::load(&dir.join("empty.yaml")).unwrap();
    assert_eq!(evm_project_sol(&a), evm_project_sol(&b));
}

#[test]
fn yaml_unused_library_paths_do_not_change_evm() {
    let root = tempdir();
    let proj = root.join("proj");
    std::fs::create_dir_all(root.join("unused_lib")).unwrap();
    std::fs::create_dir_all(&proj).unwrap();
    write_rel(&proj, "math.cam", LIB_DOUBLE);
    write_rel(
        &proj,
        "Foo.cam",
        &(r#"import "./math.cam""#.to_string() + FOO_USES_DOUBLE),
    );
    write_rel(&proj, "legacy.yaml", "target: evm\nsources:\n  - Foo.cam\n");
    write_rel(
        &proj,
        "with_roots.yaml",
        "target: evm\n\
         sources:\n\
         \x20\x20- Foo.cam\n\
         library_paths:\n\
         \x20\x20- ../unused_lib\n",
    );

    let a = Project::load(&proj.join("legacy.yaml")).unwrap();
    let b = Project::load(&proj.join("with_roots.yaml")).unwrap();
    assert_eq!(evm_project_sol(&a), evm_project_sol(&b));
}

#[test]
fn yaml_library_paths_expand_env() {
    let root = tempdir();
    let proj = root.join("proj");
    let lib = root.join("lib");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::create_dir_all(&lib).unwrap();
    write_rel(&lib, "math.cam", LIB_DOUBLE);
    write_rel(&proj, "Foo.cam", FOO_USES_DOUBLE);
    let key = format!(
        "CAMBRIAN_TEST_STDLIB_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    std::env::set_var(&key, &lib);
    write_rel(
        &proj,
        "project.yaml",
        &format!(
            "target: evm\n\
             sources:\n\
             \x20\x20- Foo.cam\n\
             library_paths:\n\
             \x20\x20- ${{{key}}}\n\
             imports:\n\
             \x20\x20- math.cam\n"
        ),
    );

    let project = Project::load(&proj.join("project.yaml")).unwrap();
    std::env::remove_var(&key);
    assert!(project.merged.pure_fns.iter().any(|f| f.name == "double"));
}

#[test]
fn yaml_unset_env_in_library_paths_does_not_collapse() {
    let dir = tempdir();
    write_rel(&dir, "Foo.cam", FOO_PASSTHROUGH);
    write_rel(
        &dir,
        "project.yaml",
        "target: evm\n\
         sources:\n\
         \x20\x20- Foo.cam\n\
         library_paths:\n\
         \x20\x20- ${CAMBRIAN_TEST_UNSET_NO_SUCH_VAR}/stdlib\n\
         imports:\n\
         \x20\x20- math.cam\n",
    );

    match Project::load(&dir.join("project.yaml")) {
        Err(ProjectError::Packaging { code, message, .. }) => {
            assert_eq!(code, "F5");
            assert!(
                message.contains("${CAMBRIAN_TEST_UNSET_NO_SUCH_VAR}"),
                "unset ${{VAR}} must stay literal in F5, got: {message}"
            );
        }
        other => panic!("expected F5 Packaging, got {other:?}"),
    }
}

// ===========================================================================
// Helpers
// ===========================================================================

fn tempdir() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let base = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("tmp");
    let id = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = base.join(format!("cambrian_test_{}_{}", std::process::id(), id));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
