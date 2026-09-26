// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Validator ↔ codegen contract audit tests (Phase E).
//!
//! Each case loads a fixture project, runs the same validation pipeline as
//! the CLI (`validate` + target-compat checks), then exercises codegen (Forge
//! for EVM, `lake build` for Lean L-rules). A failing assertion means the
//! hypothesis is **confirmed**.
//!
//! See [docs/AUDIT_EVM_LEAN.md](../../../docs/AUDIT_EVM_LEAN.md) §6 Phase E.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use cambrian_transpiler::codegen::{EvmSolidityBackend, LeanBackend, OutputBackend};
use cambrian_transpiler::project::Project;
use cambrian_transpiler::target::Target;
use cambrian_transpiler::validate::{
    self, check_evm_target_compat_with, check_lean_target_compat, check_target_compat,
    validate_project_config, Diagnostic, Severity,
};

const FOUNDRY_TOML: &str = r#"[profile.default]
src = "src"
out = "out"
libs = ["lib"]
solc_version = "0.8.24"
evm_version = "prague"
optimizer = false
optimizer_runs = 200
via_ir = false
"#;

const LAKE_BUILD_TIMEOUT: Duration = Duration::from_secs(300);

static OUT_COUNTER: AtomicU64 = AtomicU64::new(0);

enum ValidatorAuditKind {
    /// E07 warns on EVM-12 filter.map.fold while Forge passes.
    StaleE07FoldChain,
    /// E23 accepts pure-fn tuple destructure; codegen must type each slot.
    E23MixedTuplePureFn {
        correct_destructure: &'static str,
        widened_head: &'static str,
    },
    /// E15: `gosh::*` in expression position must be rejected on EVM target.
    E15GoshExprReject {
        control_yaml: &'static str,
    },
    /// E16: `address_of` outside deterministic mode must be rejected on EVM target.
    E16AddressOfReject {
        control_yaml: &'static str,
    },
    /// E17: unrecognised HashMap member-transform shape must be rejected on EVM target.
    E17HashMapTransformReject {
        control_yaml: &'static str,
    },
    /// E18: tuple type outside multi-return position must be rejected on EVM target.
    E18TupleTypeReject {
        control_yaml: &'static str,
    },
    /// E19: unknown generic type must be rejected on EVM target.
    E19UnknownGenericReject {
        control_yaml: &'static str,
    },
    /// E20: unknown bare type identifier must be rejected on EVM target.
    E20UnknownSimpleReject {
        control_yaml: &'static str,
    },
    /// E21: invalid `as <Type>` cast target must be rejected on EVM target.
    E21InvalidCastReject {
        control_yaml: &'static str,
    },
    /// V49: bare stdlib call (e.g. `min`) must be rejected — use `std::math::min`.
    V49BareStdlibReject {
        std_hint: &'static str,
        control_yaml: &'static str,
    },
    /// V50: implicit String→numeric member transform without `std::str::parse_uint`.
    V50ImplicitStringToNumeric {
        std_hint: &'static str,
        control_yaml: &'static str,
    },
    /// L13: `std::crypto::*` has no Lean lowering — must reject at validate time.
    L13StdCryptoReject {
        std_hint: &'static str,
    },
    /// Lean L-rule rejects program; forced Lean must not `lake build` clean.
    LeanLRuleContract {
        rule: &'static str,
        routes_file: &'static str,
        route_name: &'static str,
        spec_file: Option<&'static str>,
        mode: LeanLRuleContractMode,
    },
}

enum LeanLRuleContractMode {
    /// L11 / L9: silent `exceptGetD` recovery on internal failing calls / captures.
    SilentRecovery {
        bad_lowering: &'static str,
        good_lowering: &'static str,
    },
    /// L8: unresolved / unsynthesisable typed send must not silent-skip or compile clean.
    UnresolvedSend {
        l8_sentinel: &'static str,
        send_markers: &'static [&'static str],
    },
    /// L10: invariant `step` on a sending route must not hide interleaving without sentinel.
    InvariantSendWarning {
        action_name: &'static str,
        l10_sentinel: &'static str,
    },
}

struct ValidatorAuditCase {
    id: &'static str,
    hypothesis: &'static str,
    project_yaml: &'static str,
    forge_test_file: Option<&'static str>,
    forge_match_test: Option<&'static str>,
    kind: ValidatorAuditKind,
}

const CASES: &[ValidatorAuditCase] = &[
    ValidatorAuditCase {
        id: "T-VAL-001",
        hypothesis: "X-H1",
        project_yaml: "val_e07_filter_map_fold.yaml",
        forge_test_file: Some("ValE07FilterMapFold.t.sol"),
        forge_match_test: Some("test_VAL_E07_filterMapFoldSumIsTwelve"),
        kind: ValidatorAuditKind::StaleE07FoldChain,
    },
    ValidatorAuditCase {
        id: "T-VAL-002",
        hypothesis: "X-H2",
        project_yaml: "val_e23_mixed_tuple_purefn.yaml",
        forge_test_file: Some("ValE23MixedTuplePureFn.t.sol"),
        forge_match_test: Some("test_VAL_E23_mixedTuplePureFnPreservesSlots"),
        kind: ValidatorAuditKind::E23MixedTuplePureFn {
            correct_destructure: "(address owner, uint256 amt, bool flag) = split(who, seed);",
            widened_head: "(uint256 owner, uint256 amt",
        },
    },
    ValidatorAuditCase {
        id: "T-VAL-004",
        hypothesis: "E15-gosh-expr",
        project_yaml: "val_e15_gosh_expr.yaml",
        forge_test_file: None,
        forge_match_test: None,
        kind: ValidatorAuditKind::E15GoshExprReject {
            control_yaml: "val_e15_evm_ns_control.yaml",
        },
    },
    ValidatorAuditCase {
        id: "T-VAL-005",
        hypothesis: "E16-address-of",
        project_yaml: "val_e16_address_of.yaml",
        forge_test_file: None,
        forge_match_test: None,
        kind: ValidatorAuditKind::E16AddressOfReject {
            control_yaml: "val_e16_address_of_det_control.yaml",
        },
    },
    ValidatorAuditCase {
        id: "T-VAL-006",
        hypothesis: "E17-hashmap-transform",
        project_yaml: "val_e17_hashmap_shape.yaml",
        forge_test_file: None,
        forge_match_test: None,
        kind: ValidatorAuditKind::E17HashMapTransformReject {
            control_yaml: "val_e17_hashmap_insert_control.yaml",
        },
    },
    ValidatorAuditCase {
        id: "T-VAL-007",
        hypothesis: "E18-tuple-type",
        project_yaml: "val_e18_tuple_storage.yaml",
        forge_test_file: None,
        forge_match_test: None,
        kind: ValidatorAuditKind::E18TupleTypeReject {
            control_yaml: "val_e18_tuple_return_control.yaml",
        },
    },
    ValidatorAuditCase {
        id: "T-VAL-008",
        hypothesis: "E19-unknown-generic",
        project_yaml: "val_e19_unknown_generic.yaml",
        forge_test_file: None,
        forge_match_test: None,
        kind: ValidatorAuditKind::E19UnknownGenericReject {
            control_yaml: "val_e19_known_generics_control.yaml",
        },
    },
    ValidatorAuditCase {
        id: "T-VAL-009",
        hypothesis: "E20-unknown-simple",
        project_yaml: "val_e20_unknown_simple.yaml",
        forge_test_file: None,
        forge_match_test: None,
        kind: ValidatorAuditKind::E20UnknownSimpleReject {
            control_yaml: "val_e20_type_alias_control.yaml",
        },
    },
    ValidatorAuditCase {
        id: "T-VAL-010",
        hypothesis: "E21-invalid-cast",
        project_yaml: "val_e21_invalid_cast.yaml",
        forge_test_file: None,
        forge_match_test: None,
        kind: ValidatorAuditKind::E21InvalidCastReject {
            control_yaml: "val_e21_alias_cast_control.yaml",
        },
    },
    ValidatorAuditCase {
        id: "T-STD-VAL-001",
        hypothesis: "STD-H-VAL-1",
        project_yaml: "validator/std_val_001_bare_min.yaml",
        forge_test_file: None,
        forge_match_test: None,
        kind: ValidatorAuditKind::V49BareStdlibReject {
            std_hint: "std::math::min",
            control_yaml: "validator/std_val_001_std_min_control.yaml",
        },
    },
    ValidatorAuditCase {
        id: "T-STD-VAL-002",
        hypothesis: "STD-H-VAL-2",
        project_yaml: "validator/std_val_002_string_to_u64.yaml",
        forge_test_file: None,
        forge_match_test: None,
        kind: ValidatorAuditKind::V50ImplicitStringToNumeric {
            std_hint: "std::str::parse_uint",
            control_yaml: "validator/std_val_002_parse_uint_control.yaml",
        },
    },
    ValidatorAuditCase {
        id: "T-STD-VAL-003",
        hypothesis: "STD-H-VAL-1",
        project_yaml: "validator/std_val_003_bare_sha256.yaml",
        forge_test_file: None,
        forge_match_test: None,
        kind: ValidatorAuditKind::V49BareStdlibReject {
            std_hint: "std::crypto::sha256",
            control_yaml: "validator/std_val_003_std_crypto_control.yaml",
        },
    },
    ValidatorAuditCase {
        id: "T-STD-VAL-004",
        hypothesis: "STD-H-LEAN-2",
        project_yaml: "validator/std_val_004_std_crypto_lean.yaml",
        forge_test_file: None,
        forge_match_test: None,
        kind: ValidatorAuditKind::L13StdCryptoReject {
            std_hint: "std::crypto",
        },
    },
    ValidatorAuditCase {
        id: "T-VAL-003",
        hypothesis: "X-H3",
        project_yaml: "val_l11_failing_call_lean.yaml",
        forge_test_file: None,
        forge_match_test: None,
        kind: ValidatorAuditKind::LeanLRuleContract {
            rule: "L11",
            routes_file: "Cambrian/Generated/FailCallRoutes.lean",
            route_name: "invoke",
            spec_file: None,
            mode: LeanLRuleContractMode::SilentRecovery {
                bad_lowering: "Cambrian.exceptGetD",
                good_lowering: "Cambrian.RouteResult",
            },
        },
    },
    ValidatorAuditCase {
        id: "T-VAL-003b-L8",
        hypothesis: "X-H3",
        project_yaml: "val_l8_unresolved_send_lean.yaml",
        forge_test_file: None,
        forge_match_test: None,
        kind: ValidatorAuditKind::LeanLRuleContract {
            rule: "L8",
            routes_file: "Cambrian/Generated/SenderRoutes.lean",
            route_name: "pay",
            spec_file: None,
            mode: LeanLRuleContractMode::UnresolvedSend {
                l8_sentinel: "-- L8:",
                send_markers: &[
                    "Cambrian.Generated.Dispatch",
                    "Treasury.Routes",
                    "WorldState.transfer",
                ],
            },
        },
    },
    ValidatorAuditCase {
        id: "T-VAL-003b-L9",
        hypothesis: "X-H3",
        project_yaml: "val_l9_failing_capture_lean.yaml",
        forge_test_file: None,
        forge_match_test: None,
        kind: ValidatorAuditKind::LeanLRuleContract {
            rule: "L9",
            routes_file: "Cambrian/Generated/CallerRoutes.lean",
            route_name: "bad_capture",
            spec_file: None,
            mode: LeanLRuleContractMode::SilentRecovery {
                bad_lowering: "Cambrian.exceptGetD",
                good_lowering: ".ok (",
            },
        },
    },
    ValidatorAuditCase {
        id: "T-VAL-003b-L10",
        hypothesis: "X-H3",
        project_yaml: "val_l10_invariant_send_lean.yaml",
        forge_test_file: None,
        forge_match_test: None,
        kind: ValidatorAuditKind::LeanLRuleContract {
            rule: "L10",
            routes_file: "Cambrian/Generated/PingRoutes.lean",
            route_name: "kick",
            spec_file: Some("Cambrian/Generated/PingSpec.lean"),
            mode: LeanLRuleContractMode::InvariantSendWarning {
                action_name: "kick",
                l10_sentinel: "-- L10:",
            },
        },
    },
];

fn audit_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/audit")
}

fn project_yaml_path(yaml: &str) -> PathBuf {
    let direct = audit_root().join(yaml);
    if direct.exists() {
        direct
    } else {
        audit_root().join("fixtures").join(yaml)
    }
}

fn unique_out_dir(tag: &str) -> PathBuf {
    let n = OUT_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cambrian-audit-val-{}-{}-{}",
        tag,
        std::process::id(),
        n
    ))
}

fn has_forge() -> bool {
    Command::new("forge")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn has_lake() -> bool {
    Command::new("lake")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn lean_build_enabled() -> bool {
    std::env::var("CAMBRIAN_TEST_LEAN_BUILD").as_deref() == Ok("1")
}

fn format_diagnostic(d: &Diagnostic) -> String {
    format!("[{}] {}", d.code, d.message)
}

/// EVM target-compat `det` for validator audits. E16 probes nondet rejection
/// (`address_of` without CREATE2) even though project yaml resolves det=true.
fn evm_compat_det(project: &Project, kind: Option<&ValidatorAuditKind>) -> bool {
    if matches!(kind, Some(ValidatorAuditKind::E16AddressOfReject { .. })) {
        return false;
    }
    project.config.resolved_deterministic_addresses()
}

fn collect_evm_diagnostics(project: &Project, kind: Option<&ValidatorAuditKind>) -> Vec<Diagnostic> {
    let mut diags = validate::validate(&project.merged);
    diags.extend(validate_project_config(&project.config));
    let det = evm_compat_det(project, kind);
    diags.extend(check_evm_target_compat_with(&project.merged, det));
    diags
}

fn collect_lean_diagnostics(project: &Project) -> Vec<Diagnostic> {
    let mut diags = validate::validate(&project.merged);
    diags.extend(validate_project_config(&project.config));
    diags.extend(check_lean_target_compat(&project.merged));
    diags
}

fn has_fold_chain_e07(diags: &[Diagnostic]) -> bool {
    diags.iter().any(|d| {
        d.code == "E07"
            && d.message.contains(".fold` over an iterator-method chain")
    })
}

fn has_e23(diags: &[Diagnostic]) -> bool {
    diags
        .iter()
        .any(|d| d.code == "E23" && matches!(d.severity, Severity::Error))
}

fn has_e15(diags: &[Diagnostic]) -> bool {
    diags
        .iter()
        .any(|d| d.code == "E15" && matches!(d.severity, Severity::Error))
}

fn collect_evm_diagnostics_from_yaml(yaml: &str) -> Vec<Diagnostic> {
    let yaml_path = project_yaml_path(yaml);
    let project = Project::load(&yaml_path).expect("load control fixture");
    collect_evm_diagnostics(&project, None)
}

fn has_v49(diags: &[Diagnostic], hint: &str) -> bool {
    diags.iter().any(|d| {
        d.code == "V49"
            && matches!(d.severity, Severity::Error)
            && d.message.contains(hint)
    })
}

fn v49_bare_stdlib_silent_miscompile(sol: &str) -> bool {
    !sol.contains("forced EVM transpile skipped")
}

fn has_v50(diags: &[Diagnostic], hint: &str) -> bool {
    diags.iter().any(|d| {
        d.code == "V50"
            && matches!(d.severity, Severity::Error)
            && d.message.contains(hint)
    })
}

/// V50 diagnostics always suggest `std::str::parse_uint` in the message text — use
/// this for implicit String→numeric probes (T-STD-VAL-002), not substring `std_hint`.
fn has_v50_implicit_string_to_numeric(diags: &[Diagnostic]) -> bool {
    diags.iter().any(|d| {
        d.code == "V50"
            && matches!(d.severity, Severity::Error)
            && d.message.contains("type `String`")
    })
}

fn v50_implicit_string_silent_miscompile(sol: &str) -> bool {
    !sol.contains("forced EVM transpile skipped")
}

fn has_l13(diags: &[Diagnostic], hint: &str) -> bool {
    diags.iter().any(|d| {
        d.code == "L13"
            && matches!(d.severity, Severity::Error)
            && d.message.contains(hint)
    })
}

fn gosh_expr_silent_miscompile(sol: &str) -> bool {
    sol.contains("should have been rejected by E15")
        || sol.contains("EVM-2 K3: `gosh::")
}

fn has_e16(diags: &[Diagnostic]) -> bool {
    diags
        .iter()
        .any(|d| d.code == "E16" && matches!(d.severity, Severity::Error))
}

fn address_of_silent_miscompile(sol: &str) -> bool {
    sol.contains("should have been rejected by E16")
        || sol.contains("EVM-2 K3: unsupported expression")
        || sol.contains("debug_assert on address_of")
}

fn has_e17(diags: &[Diagnostic]) -> bool {
    diags
        .iter()
        .any(|d| d.code == "E17" && matches!(d.severity, Severity::Error))
}

fn hashmap_transform_silent_miscompile(sol: &str) -> bool {
    sol.contains("should have been rejected by E17")
        || sol.contains("mapping `") && sol.contains("transform shape should have been rejected by E17")
        || sol.contains("transform: no-op")
        || sol.contains("debug_assert on HashMap transform")
}

fn has_e18(diags: &[Diagnostic]) -> bool {
    diags
        .iter()
        .any(|d| d.code == "E18" && matches!(d.severity, Severity::Error))
}

fn tuple_type_silent_miscompile(sol: &str) -> bool {
    sol.contains("should have been rejected by E18")
        || sol.contains("bytes public m_pair")
        || sol.contains("bytes m_pair")
}

fn has_e19(diags: &[Diagnostic]) -> bool {
    diags
        .iter()
        .any(|d| d.code == "E19" && matches!(d.severity, Severity::Error))
}

fn unknown_generic_silent_miscompile(sol: &str) -> bool {
    sol.contains("should have been rejected by E19")
        || sol.contains("bytes public m_box")
        || sol.contains("bytes m_box")
}

fn has_e20(diags: &[Diagnostic]) -> bool {
    diags
        .iter()
        .any(|d| d.code == "E20" && matches!(d.severity, Severity::Error))
}

fn unknown_simple_silent_miscompile(sol: &str) -> bool {
    sol.contains("should have been rejected by E20")
        || sol.contains("uint256 public m_unknown")
        || sol.contains("uint256 m_unknown")
}

fn has_e21(diags: &[Diagnostic]) -> bool {
    diags
        .iter()
        .any(|d| d.code == "E21" && matches!(d.severity, Severity::Error))
}

fn invalid_cast_silent_miscompile(sol: &str) -> bool {
    sol.contains("should have been rejected by E21")
        || sol.contains("string s = 0")
        || sol.contains("string memory s = 0")
}

fn evm_validator_reject_present(kind: &ValidatorAuditKind, diags: &[Diagnostic]) -> bool {
    match kind {
        ValidatorAuditKind::E15GoshExprReject { .. } => has_e15(diags),
        ValidatorAuditKind::E16AddressOfReject { .. } => has_e16(diags),
        ValidatorAuditKind::E17HashMapTransformReject { .. } => has_e17(diags),
        ValidatorAuditKind::E18TupleTypeReject { .. } => has_e18(diags),
        ValidatorAuditKind::E19UnknownGenericReject { .. } => has_e19(diags),
        ValidatorAuditKind::E20UnknownSimpleReject { .. } => has_e20(diags),
        ValidatorAuditKind::E21InvalidCastReject { .. } => has_e21(diags),
        ValidatorAuditKind::V49BareStdlibReject { std_hint, .. } => has_v49(diags, std_hint),
        ValidatorAuditKind::V50ImplicitStringToNumeric { std_hint, .. } => has_v50(diags, std_hint),
        _ => false,
    }
}

fn evm_k3_debug_assert_breach_kind(kind: &ValidatorAuditKind) -> bool {
    matches!(
        kind,
        ValidatorAuditKind::E16AddressOfReject { .. }
            | ValidatorAuditKind::E17HashMapTransformReject { .. }
    )
}

fn evm_k3_debug_assert_breach_label(kind: &ValidatorAuditKind) -> &'static str {
    match kind {
        ValidatorAuditKind::E16AddressOfReject { .. } => {
            "(EVM-2 K3 debug_assert on address_of — validator E16 absent)"
        }
        ValidatorAuditKind::E17HashMapTransformReject { .. } => {
            "(EVM-2 K3 debug_assert on HashMap transform — validator E17 absent)"
        }
        _ => "(EVM-2 K3 debug_assert — validator absent)",
    }
}

/// Forced EVM transpile for validator reject contracts. When the probe
/// validator error is present, skip codegen (E16/E17 hit `debug_assert`
/// in debug builds). When absent, transpile to detect silent K3 sentinel
/// — catch unwind from defence-in-depth.
fn force_evm_transpile_for_audit(
    kind: &ValidatorAuditKind,
    project: &Project,
    out_dir: &Path,
    diags: &[Diagnostic],
) -> Result<String, String> {
    if evm_validator_reject_present(kind, diags) {
        return Ok("(forced EVM transpile skipped — validator reject present)".into());
    }

    if evm_k3_debug_assert_breach_kind(kind) {
        return match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            transpile_evm(project, out_dir, Some(kind))?;
            Ok(read_generated_sol(out_dir))
        })) {
            Ok(Ok(sol)) => Ok(sol),
            Ok(Err(err)) => Err(err),
            Err(_) => Ok(evm_k3_debug_assert_breach_label(kind).into()),
        };
    }

    transpile_evm(project, out_dir, Some(kind))?;
    Ok(read_generated_sol(out_dir))
}

fn has_lean_rule_error(diags: &[Diagnostic], rule: &str) -> bool {
    diags
        .iter()
        .any(|d| d.code == rule && matches!(d.severity, Severity::Error))
}

fn has_lean_rule_warning(diags: &[Diagnostic], rule: &str) -> bool {
    diags
        .iter()
        .any(|d| d.code == rule && matches!(d.severity, Severity::Warning))
}

fn lean_rule_present(diags: &[Diagnostic], rule: &str, mode: &LeanLRuleContractMode) -> bool {
    match mode {
        LeanLRuleContractMode::InvariantSendWarning { .. } => {
            has_lean_rule_warning(diags, rule)
        }
        _ => has_lean_rule_error(diags, rule),
    }
}

fn ensure_forge_std(out_dir: &Path) {
    cambrian_transpiler::codegen::evm_test_codegen::install_forge_std(out_dir)
        .unwrap_or_else(|e| panic!("{e} in {}", out_dir.display()));
}

fn write_codegen_files(
    files: impl IntoIterator<Item = (String, String)>,
    out_dir: &Path,
) -> Result<(), String> {
    for (rel, contents) in files {
        let path = out_dir.join(&rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        std::fs::write(&path, contents).map_err(|e| format!("write {}: {e}", path.display()))?;
    }
    Ok(())
}

fn transpile_evm(
    project: &Project,
    out_dir: &Path,
    kind: Option<&ValidatorAuditKind>,
) -> Result<(), String> {
    let det = evm_compat_det(project, kind);
    let backend = EvmSolidityBackend {
        deterministic_addresses: det,
    };
    write_codegen_files(backend.gen_project(project), out_dir)
}

fn transpile_lean(project: &Project, out_dir: &Path) -> Result<(), String> {
    let backend = LeanBackend::default();
    write_codegen_files(backend.gen_project(project), out_dir)
}

fn read_generated_sol(out_dir: &Path) -> String {
    let src = out_dir.join("src");
    fs::read_dir(&src)
        .expect("src dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "sol"))
        .map(|p| fs::read_to_string(p).expect("read sol"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn read_generated_lean(out_dir: &Path, rel: &str) -> String {
    fs::read_to_string(out_dir.join(rel)).unwrap_or_else(|e| {
        panic!("read generated Lean {}: {e}", out_dir.join(rel).display())
    })
}

fn extract_route_def_snippet(lean: &str, route_name: &str) -> String {
    let needle = format!("def {route_name} ");
    let start = lean
        .find(&needle)
        .unwrap_or_else(|| panic!("route `{route_name}` not found in generated Lean"));
    let rest = &lean[start..];
    let end = rest.find("\n\n").unwrap_or(rest.len());
    rest[..end].lines().take(8).collect::<Vec<_>>().join("\n")
}

fn extract_spec_step_snippet(spec: &str, action_name: &str) -> String {
    let needle = format!("| .{action_name} ");
    let start = spec.find(&needle).unwrap_or_else(|| {
        panic!("invariant step arm `.{action_name}` not found in generated Spec.lean")
    });
    let rest = &spec[start..];
    let end = rest
        .find("\n  |")
        .or_else(|| rest.find("\n\n"))
        .unwrap_or(rest.len());
    rest[..end].lines().take(4).collect::<Vec<_>>().join("\n")
}

struct ForgeRun {
    ok: bool,
    combined: String,
}

fn run_forge(case: &ValidatorAuditCase, out_dir: &Path) -> ForgeRun {
    let test_file = case
        .forge_test_file
        .expect("EVM case must set forge_test_file");
    let match_test = case
        .forge_match_test
        .expect("EVM case must set forge_match_test");
    let test_dir = out_dir.join("test");
    std::fs::create_dir_all(&test_dir).expect("test dir");
    let src_test = audit_root().join("forge").join(test_file);
    std::fs::copy(&src_test, test_dir.join(test_file)).expect("copy forge test");
    ensure_forge_std(out_dir);

    let forge = Command::new("forge")
        .args(["test", "--match-test", match_test, "-vv", "--root"])
        .arg(out_dir)
        .output()
        .expect("forge test");

    ForgeRun {
        ok: forge.status.success(),
        combined: format!(
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&forge.stdout),
            String::from_utf8_lossy(&forge.stderr)
        ),
    }
}

struct LakeRun {
    ran: bool,
    ok: bool,
    combined: String,
}

fn run_lake(out_dir: &Path) -> LakeRun {
    if !lean_build_enabled() {
        return LakeRun {
            ran: false,
            ok: false,
            combined: "skipped (set CAMBRIAN_TEST_LEAN_BUILD=1 to enable)".into(),
        };
    }
    if !has_lake() {
        panic!("CAMBRIAN_TEST_LEAN_BUILD=1 set but `lake` not on PATH");
    }

    let lake = Command::new("lake")
        .arg("build")
        .current_dir(out_dir)
        .output()
        .expect("invoke lake");

    LakeRun {
        ran: true,
        ok: lake.status.success(),
        combined: format!(
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&lake.stdout),
            String::from_utf8_lossy(&lake.stderr)
        ),
    }
}

struct ValidatorRun {
    ok: bool,
    detail: String,
    confirmed: bool,
}

fn evaluate_evm_case(
    case: &ValidatorAuditCase,
    diags: &[Diagnostic],
    sol: &str,
    forge: &ForgeRun,
) -> ValidatorRun {
    match &case.kind {
        ValidatorAuditKind::StaleE07FoldChain => {
            let e07_fold_chain = has_fold_chain_e07(diags);
            let confirmed = forge.ok && e07_fold_chain;
            let e07_text: String = diags
                .iter()
                .filter(|d| d.code == "E07")
                .map(format_diagnostic)
                .collect::<Vec<_>>()
                .join("\n");
            ValidatorRun {
                ok: forge.ok && !e07_fold_chain,
                confirmed,
                detail: format!(
                    "Validator E07 (fold-chain): {}\n\nE07 diagnostics:\n{}\n\nForge: {}\n{}\n\nStale validator: {}",
                    if e07_fold_chain { "PRESENT" } else { "absent" },
                    if e07_text.is_empty() { "(none)".into() } else { e07_text },
                    if forge.ok { "PASS" } else { "FAIL" },
                    forge.combined,
                    if confirmed { "YES" } else { "no" },
                ),
            }
        }
        ValidatorAuditKind::E23MixedTuplePureFn {
            correct_destructure,
            widened_head,
        } => {
            let e23 = has_e23(diags);
            let typed_ok = sol.contains(correct_destructure);
            let widened = sol.contains(widened_head);
            let confirmed = !e23 && (widened || !forge.ok || !typed_ok);
            let disproven = e23 || (forge.ok && typed_ok && !widened);
            ValidatorRun {
                ok: disproven,
                confirmed,
                detail: format!(
                    "Validator E23: {}\nCodegen typed destructure: {}\nCodegen widened head: {}\n\nExpected destructure:\n  {}\nForbidden widen:\n  {}\n\nForge: {}\n{}\n\nContract gap (accepted + bad lowering): {}",
                    if e23 { "PRESENT (rejected)" } else { "absent (accepted)" },
                    if typed_ok { "yes" } else { "NO" },
                    if widened { "YES" } else { "no" },
                    correct_destructure,
                    widened_head,
                    if forge.ok { "PASS" } else { "FAIL" },
                    forge.combined,
                    if confirmed { "YES" } else { "no" },
                ),
            }
        }
        ValidatorAuditKind::E15GoshExprReject { control_yaml } => {
            let e15 = has_e15(diags);
            let control_e15 = has_e15(&collect_evm_diagnostics_from_yaml(control_yaml));
            let silent_bad = gosh_expr_silent_miscompile(sol);
            let confirmed = !e15 && silent_bad;
            let aligned = e15 && !control_e15;
            let e15_text: String = diags
                .iter()
                .filter(|d| d.code == "E15")
                .map(format_diagnostic)
                .collect::<Vec<_>>()
                .join("\n");
            ValidatorRun {
                ok: aligned,
                confirmed,
                detail: format!(
                    "Validator E15 (gosh expr): {}\nControl fixture E15: {}\nCodegen silent miscompile marker: {}\n\nE15 diagnostics:\n{}\n\nForced EVM codegen snippet (tail):\n{}\n\nContract breach (accepted + silent gosh expr lowering): {}",
                    if e15 { "PRESENT" } else { "absent" },
                    if control_e15 { "PRESENT (bad control)" } else { "absent (ok)" },
                    if silent_bad { "YES" } else { "no" },
                    if e15_text.is_empty() { "(none)".into() } else { e15_text },
                    sol.lines().rev().take(20).collect::<Vec<_>>().join("\n"),
                    if confirmed { "YES" } else if aligned { "no (ALIGNED)" } else { "INCONCLUSIVE" },
                ),
            }
        }
        ValidatorAuditKind::E16AddressOfReject { control_yaml } => {
            let e16 = has_e16(diags);
            let control_e16 = has_e16(&collect_evm_diagnostics_from_yaml(control_yaml));
            let silent_bad = address_of_silent_miscompile(sol);
            let confirmed = !e16 && silent_bad;
            let aligned = e16 && !control_e16;
            let e16_text: String = diags
                .iter()
                .filter(|d| d.code == "E16")
                .map(format_diagnostic)
                .collect::<Vec<_>>()
                .join("\n");
            ValidatorRun {
                ok: aligned,
                confirmed,
                detail: format!(
                    "Validator E16 (address_of): {}\nControl fixture E16: {}\nCodegen silent miscompile marker: {}\n\nE16 diagnostics:\n{}\n\nForced EVM codegen snippet (tail):\n{}\n\nContract breach (accepted + silent address_of lowering): {}",
                    if e16 { "PRESENT" } else { "absent" },
                    if control_e16 { "PRESENT (bad control)" } else { "absent (ok)" },
                    if silent_bad { "YES" } else { "no" },
                    if e16_text.is_empty() { "(none)".into() } else { e16_text },
                    sol.lines().rev().take(20).collect::<Vec<_>>().join("\n"),
                    if confirmed { "YES" } else if aligned { "no (ALIGNED)" } else { "INCONCLUSIVE" },
                ),
            }
        }
        ValidatorAuditKind::E17HashMapTransformReject { control_yaml } => {
            let e17 = has_e17(diags);
            let control_e17 = has_e17(&collect_evm_diagnostics_from_yaml(control_yaml));
            let silent_bad = hashmap_transform_silent_miscompile(sol);
            let confirmed = !e17 && silent_bad;
            let aligned = e17 && !control_e17;
            let e17_text: String = diags
                .iter()
                .filter(|d| d.code == "E17")
                .map(format_diagnostic)
                .collect::<Vec<_>>()
                .join("\n");
            ValidatorRun {
                ok: aligned,
                confirmed,
                detail: format!(
                    "Validator E17 (HashMap transform): {}\nControl fixture E17: {}\nCodegen silent miscompile marker: {}\n\nE17 diagnostics:\n{}\n\nForced EVM codegen snippet (tail):\n{}\n\nContract breach (accepted + silent HashMap transform lowering): {}",
                    if e17 { "PRESENT" } else { "absent" },
                    if control_e17 { "PRESENT (bad control)" } else { "absent (ok)" },
                    if silent_bad { "YES" } else { "no" },
                    if e17_text.is_empty() { "(none)".into() } else { e17_text },
                    sol.lines().rev().take(20).collect::<Vec<_>>().join("\n"),
                    if confirmed { "YES" } else if aligned { "no (ALIGNED)" } else { "INCONCLUSIVE" },
                ),
            }
        }
        ValidatorAuditKind::E18TupleTypeReject { control_yaml } => {
            let e18 = has_e18(diags);
            let control_e18 = has_e18(&collect_evm_diagnostics_from_yaml(control_yaml));
            let silent_bad = tuple_type_silent_miscompile(sol);
            let confirmed = !e18 && silent_bad;
            let aligned = e18 && !control_e18;
            let e18_text: String = diags
                .iter()
                .filter(|d| d.code == "E18")
                .map(format_diagnostic)
                .collect::<Vec<_>>()
                .join("\n");
            ValidatorRun {
                ok: aligned,
                confirmed,
                detail: format!(
                    "Validator E18 (tuple type): {}\nControl fixture E18: {}\nCodegen silent miscompile marker: {}\n\nE18 diagnostics:\n{}\n\nForced EVM codegen snippet (tail):\n{}\n\nContract breach (accepted + silent tuple-to-bytes erasure): {}",
                    if e18 { "PRESENT" } else { "absent" },
                    if control_e18 { "PRESENT (bad control)" } else { "absent (ok)" },
                    if silent_bad { "YES" } else { "no" },
                    if e18_text.is_empty() { "(none)".into() } else { e18_text },
                    sol.lines().rev().take(20).collect::<Vec<_>>().join("\n"),
                    if confirmed { "YES" } else if aligned { "no (ALIGNED)" } else { "INCONCLUSIVE" },
                ),
            }
        }
        ValidatorAuditKind::E19UnknownGenericReject { control_yaml } => {
            let e19 = has_e19(diags);
            let control_e19 = has_e19(&collect_evm_diagnostics_from_yaml(control_yaml));
            let silent_bad = unknown_generic_silent_miscompile(sol);
            let confirmed = !e19 && silent_bad;
            let aligned = e19 && !control_e19;
            let e19_text: String = diags
                .iter()
                .filter(|d| d.code == "E19")
                .map(format_diagnostic)
                .collect::<Vec<_>>()
                .join("\n");
            ValidatorRun {
                ok: aligned,
                confirmed,
                detail: format!(
                    "Validator E19 (unknown generic): {}\nControl fixture E19: {}\nCodegen silent miscompile marker: {}\n\nE19 diagnostics:\n{}\n\nForced EVM codegen snippet (tail):\n{}\n\nContract breach (accepted + silent generic-to-bytes erasure): {}",
                    if e19 { "PRESENT" } else { "absent" },
                    if control_e19 { "PRESENT (bad control)" } else { "absent (ok)" },
                    if silent_bad { "YES" } else { "no" },
                    if e19_text.is_empty() { "(none)".into() } else { e19_text },
                    sol.lines().rev().take(20).collect::<Vec<_>>().join("\n"),
                    if confirmed { "YES" } else if aligned { "no (ALIGNED)" } else { "INCONCLUSIVE" },
                ),
            }
        }
        ValidatorAuditKind::E20UnknownSimpleReject { control_yaml } => {
            let e20 = has_e20(diags);
            let control_e20 = has_e20(&collect_evm_diagnostics_from_yaml(control_yaml));
            let silent_bad = unknown_simple_silent_miscompile(sol);
            let confirmed = !e20 && silent_bad;
            let aligned = e20 && !control_e20;
            let e20_text: String = diags
                .iter()
                .filter(|d| d.code == "E20")
                .map(format_diagnostic)
                .collect::<Vec<_>>()
                .join("\n");
            ValidatorRun {
                ok: aligned,
                confirmed,
                detail: format!(
                    "Validator E20 (unknown simple type): {}\nControl fixture E20: {}\nCodegen silent miscompile marker: {}\n\nE20 diagnostics:\n{}\n\nForced EVM codegen snippet (tail):\n{}\n\nContract breach (accepted + silent simple-to-uint256 erasure): {}",
                    if e20 { "PRESENT" } else { "absent" },
                    if control_e20 { "PRESENT (bad control)" } else { "absent (ok)" },
                    if silent_bad { "YES" } else { "no" },
                    if e20_text.is_empty() { "(none)".into() } else { e20_text },
                    sol.lines().rev().take(20).collect::<Vec<_>>().join("\n"),
                    if confirmed { "YES" } else if aligned { "no (ALIGNED)" } else { "INCONCLUSIVE" },
                ),
            }
        }
        ValidatorAuditKind::E21InvalidCastReject { control_yaml } => {
            let e21 = has_e21(diags);
            let control_e21 = has_e21(&collect_evm_diagnostics_from_yaml(control_yaml));
            let silent_bad = invalid_cast_silent_miscompile(sol);
            let confirmed = !e21 && silent_bad;
            let aligned = e21 && !control_e21;
            let e21_text: String = diags
                .iter()
                .filter(|d| d.code == "E21")
                .map(format_diagnostic)
                .collect::<Vec<_>>()
                .join("\n");
            ValidatorRun {
                ok: aligned,
                confirmed,
                detail: format!(
                    "Validator E21 (invalid cast): {}\nControl fixture E21: {}\nCodegen silent miscompile marker: {}\n\nE21 diagnostics:\n{}\n\nForced EVM codegen snippet (tail):\n{}\n\nContract breach (accepted + silent cast identity drop): {}",
                    if e21 { "PRESENT" } else { "absent" },
                    if control_e21 { "PRESENT (bad control)" } else { "absent (ok)" },
                    if silent_bad { "YES" } else { "no" },
                    if e21_text.is_empty() { "(none)".into() } else { e21_text },
                    sol.lines().rev().take(20).collect::<Vec<_>>().join("\n"),
                    if confirmed { "YES" } else if aligned { "no (ALIGNED)" } else { "INCONCLUSIVE" },
                ),
            }
        }
        ValidatorAuditKind::V49BareStdlibReject {
            std_hint,
            control_yaml,
        } => {
            let v49 = has_v49(diags, std_hint);
            let control_v49 = has_v49(&collect_evm_diagnostics_from_yaml(control_yaml), std_hint);
            let silent_bad = v49_bare_stdlib_silent_miscompile(sol);
            let confirmed = !v49 && silent_bad;
            let aligned = v49 && !control_v49;
            let v49_text: String = diags
                .iter()
                .filter(|d| d.code == "V49")
                .map(format_diagnostic)
                .collect::<Vec<_>>()
                .join("\n");
            ValidatorRun {
                ok: aligned,
                confirmed,
                detail: format!(
                    "Validator V49 (bare stdlib): {}\nControl fixture V49: {}\nTranspile blocked (no silent codegen): {}\n\nV49 diagnostics:\n{}\n\nForced EVM codegen:\n{}\n\nContract breach (accepted bare stdlib + transpiled): {}",
                    if v49 { "PRESENT" } else { "absent" },
                    if control_v49 { "PRESENT (bad control)" } else { "absent (ok)" },
                    if v49 { "yes" } else { "NO" },
                    if v49_text.is_empty() { "(none)".into() } else { v49_text },
                    if sol.len() > 400 {
                        format!("{}…", sol.chars().take(400).collect::<String>())
                    } else {
                        sol.to_string()
                    },
                    if confirmed { "YES" } else if aligned { "no (ALIGNED / REFUTED)" } else { "INCONCLUSIVE" },
                ),
            }
        }
        ValidatorAuditKind::V50ImplicitStringToNumeric {
            control_yaml,
            ..
        } => {
            let v50 = has_v50_implicit_string_to_numeric(diags);
            let control_v50 =
                has_v50_implicit_string_to_numeric(&collect_evm_diagnostics_from_yaml(control_yaml));
            let silent_bad = v50_implicit_string_silent_miscompile(sol);
            let confirmed = !v50 && silent_bad;
            let aligned = v50 && !control_v50;
            let v50_text: String = diags
                .iter()
                .filter(|d| d.code == "V50")
                .map(format_diagnostic)
                .collect::<Vec<_>>()
                .join("\n");
            ValidatorRun {
                ok: aligned,
                confirmed,
                detail: format!(
                    "Validator V50 (implicit string→numeric): {}\nControl fixture V50: {}\nTranspile blocked (no silent codegen): {}\n\nV50 diagnostics:\n{}\n\nForced EVM codegen:\n{}\n\nContract breach (accepted implicit coercion + transpiled): {}",
                    if v50 { "PRESENT" } else { "absent" },
                    if control_v50 { "PRESENT (bad control)" } else { "absent (ok)" },
                    if v50 { "yes" } else { "NO" },
                    if v50_text.is_empty() { "(none)".into() } else { v50_text },
                    if sol.len() > 400 {
                        format!("{}…", sol.chars().take(400).collect::<String>())
                    } else {
                        sol.to_string()
                    },
                    if confirmed { "YES" } else if aligned { "no (ALIGNED / REFUTED)" } else { "INCONCLUSIVE" },
                ),
            }
        }
        ValidatorAuditKind::L13StdCryptoReject { .. } => {
            panic!("evaluate_evm_case called for L13 std::crypto case {}", case.id);
        }
        ValidatorAuditKind::LeanLRuleContract { .. } => {
            panic!("evaluate_evm_case called for Lean L-rule case {}", case.id);
        }
    }
}

fn evaluate_lean_l_rule_case(
    case: &ValidatorAuditCase,
    diags: &[Diagnostic],
    inspect_label: &str,
    inspect_snippet: &str,
    lake: &LakeRun,
) -> ValidatorRun {
    let ValidatorAuditKind::LeanLRuleContract {
        rule,
        mode,
        ..
    } = &case.kind
    else {
        panic!("evaluate_lean_l_rule_case called for non-Lean case {}", case.id);
    };

    let rule_present = lean_rule_present(diags, rule, mode);

    let rule_text: String = diags
        .iter()
        .filter(|d| d.code == *rule)
        .map(format_diagnostic)
        .collect::<Vec<_>>()
        .join("\n");

    let lake_line = if lake.ran {
        if lake.ok {
            "PASS"
        } else {
            "FAIL"
        }
    } else {
        "SKIPPED"
    };

    let (confirmed, aligned, codegen_detail) = match mode {
        LeanLRuleContractMode::SilentRecovery {
            bad_lowering,
            good_lowering,
        } => {
            let bad_in_codegen = inspect_snippet.contains(bad_lowering);
            let good_in_codegen = inspect_snippet.contains(good_lowering);
            let confirmed = rule_present && lake.ran && lake.ok;
            let aligned = rule_present && lake.ran && !lake.ok;
            let detail = format!(
                "  bad lowering {bad_lowering}: {}\n  good lowering {good_lowering}: {}",
                if bad_in_codegen { "YES" } else { "no" },
                if good_in_codegen { "yes" } else { "NO" },
            );
            (confirmed, aligned, detail)
        }
        LeanLRuleContractMode::UnresolvedSend {
            l8_sentinel,
            send_markers,
        } => {
            let has_sentinel = inspect_snippet.contains(l8_sentinel);
            let has_send_lowering = send_markers
                .iter()
                .any(|marker| inspect_snippet.contains(marker));
            let silent_bad = !has_sentinel && !has_send_lowering;
            let confirmed = rule_present && ((lake.ran && lake.ok) || silent_bad);
            let aligned = rule_present && lake.ran && !lake.ok;
            let detail = format!(
                "  `-- L8:` sentinel: {}\n  send lowering present: {}\n  silent skip (no sentinel, no send): {}",
                if has_sentinel { "yes" } else { "NO" },
                if has_send_lowering { "yes" } else { "NO" },
                if silent_bad { "YES" } else { "no" },
            );
            (confirmed, aligned, detail)
        }
        LeanLRuleContractMode::InvariantSendWarning {
            action_name,
            l10_sentinel,
        } => {
            let has_sentinel = inspect_snippet.contains(l10_sentinel);
            let atomic_route_call = inspect_snippet.contains(&format!(".Routes.{action_name}"));
            let hides_schedule = atomic_route_call && !has_sentinel;
            let confirmed = rule_present
                && (((lake.ran && lake.ok) && !has_sentinel) || hides_schedule);
            let aligned = rule_present && (has_sentinel || (lake.ran && !lake.ok));
            let detail = format!(
                "  `-- L10:` sentinel: {}\n  atomic `.Routes.{action_name}` step: {}\n  hides send schedule (atomic, no sentinel): {}",
                if has_sentinel { "yes" } else { "NO" },
                if atomic_route_call { "yes" } else { "NO" },
                if hides_schedule { "YES" } else { "no" },
            );
            (confirmed, aligned, detail)
        }
    };

    let inconclusive = !rule_present;

    let ok = if inconclusive {
        false
    } else if lake.ran {
        aligned
    } else {
        rule_present
    };

    let breach_label = match mode {
        LeanLRuleContractMode::InvariantSendWarning { .. } => {
            "Contract breach (warned + silent atomic invariant)"
        }
        _ => "Contract breach (rejected + compilable Lean)",
    };

    ValidatorRun {
        ok,
        confirmed,
        detail: format!(
            "Validator {rule}: {}\n\n{rule} diagnostics:\n{}\n\nForced codegen {inspect_label}:\n{inspect_snippet}\n{codegen_detail}\n\nlake build: {}\n{}\n\n{breach_label}: {}",
            if rule_present { "PRESENT" } else { "absent" },
            if rule_text.is_empty() { "(none)".into() } else { rule_text },
            lake_line,
            lake.combined,
            if confirmed {
                "YES (CONFIRMED)"
            } else if aligned {
                "no (ALIGNED)"
            } else if inconclusive {
                "INCONCLUSIVE (fix fixture)"
            } else {
                "unproven (lake skipped)"
            },
        ),
    }
}

fn run_evm_validator_case(case: &ValidatorAuditCase, project: &Project) -> ValidatorRun {
    let diags = collect_evm_diagnostics(project, Some(&case.kind));
    let out_dir = unique_out_dir(case.id);
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).expect("create out dir");

    let sol = match force_evm_transpile_for_audit(&case.kind, project, &out_dir, &diags) {
        Ok(sol) => sol,
        Err(err) => {
            let _ = std::fs::remove_dir_all(&out_dir);
            return ValidatorRun {
                ok: false,
                confirmed: matches!(case.kind, ValidatorAuditKind::E23MixedTuplePureFn { .. })
                    && !has_e23(&diags),
                detail: format!("transpile failed: {err}"),
            };
        }
    };
    if !out_dir.join("foundry.toml").exists() && case.forge_test_file.is_some() {
        std::fs::write(out_dir.join("foundry.toml"), FOUNDRY_TOML).expect("foundry.toml");
    }
    let forge = if case.forge_test_file.is_some() {
        run_forge(case, &out_dir)
    } else {
        ForgeRun {
            ok: true,
            combined: "(forge test skipped — validator/codegen inspect only)".into(),
        }
    };
    let run = evaluate_evm_case(case, &diags, &sol, &forge);
    let _ = std::fs::remove_dir_all(&out_dir);
    run
}

fn run_lean_l13_std_crypto_case(case: &ValidatorAuditCase, project: &Project) -> ValidatorRun {
    let ValidatorAuditKind::L13StdCryptoReject { std_hint } = &case.kind else {
        panic!("run_lean_l13_std_crypto_case called for wrong kind {}", case.id);
    };

    let diags = collect_lean_diagnostics(project);
    let l13 = has_l13(&diags, std_hint);
    let out_dir = unique_out_dir(case.id);
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).expect("create out dir");

    let lean_summary = if l13 {
        "(forced Lean transpile skipped — validator reject present)".to_string()
    } else {
        match transpile_lean(project, &out_dir) {
            Ok(()) => {
                let routes = read_generated_lean(&out_dir, "Cambrian/Generated/Pure.lean");
                if routes.contains("Cambrian.Unsupported") {
                    "transpiled with Cambrian.Unsupported sentinel".to_string()
                } else {
                    "transpiled clean (no L13)".to_string()
                }
            }
            Err(err) => format!("transpile error: {err}"),
        }
    };
    let _ = std::fs::remove_dir_all(&out_dir);

    let silent_bad = !l13 && !lean_summary.contains("forced Lean transpile skipped");
    let confirmed = !l13 && silent_bad;
    let aligned = l13;
    let l13_text: String = diags
        .iter()
        .filter(|d| d.code == "L13")
        .map(format_diagnostic)
        .collect::<Vec<_>>()
        .join("\n");

    ValidatorRun {
        ok: aligned,
        confirmed,
        detail: format!(
            "Validator L13 (std::crypto on Lean): {}\nTranspile blocked: {}\n\nL13 diagnostics:\n{}\n\nForced Lean codegen:\n{}\n\nContract breach (accepted std::crypto + transpiled): {}",
            if l13 { "PRESENT" } else { "absent" },
            if l13 { "yes" } else { "NO" },
            if l13_text.is_empty() { "(none)".into() } else { l13_text },
            lean_summary,
            if confirmed { "YES" } else if aligned { "no (ALIGNED / REFUTED)" } else { "INCONCLUSIVE" },
        ),
    }
}

fn run_lean_l_rule_case(case: &ValidatorAuditCase, project: &Project) -> ValidatorRun {
    let ValidatorAuditKind::LeanLRuleContract {
        routes_file,
        route_name,
        spec_file,
        mode,
        rule,
        ..
    } = &case.kind
    else {
        panic!("run_lean_l_rule_case called for non-Lean case {}", case.id);
    };

    let diags = collect_lean_diagnostics(project);
    let out_dir = unique_out_dir(case.id);
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).expect("create out dir");

    let codegen_result = transpile_lean(project, &out_dir);
    if let Err(err) = &codegen_result {
        let rule_present = lean_rule_present(&diags, rule, mode);
        let _ = std::fs::remove_dir_all(&out_dir);
        return ValidatorRun {
            ok: rule_present,
            confirmed: false,
            detail: format!(
                "forced codegen abort: {err}\n\nValidator {rule}: {}",
                if rule_present { "PRESENT" } else { "absent" }
            ),
        };
    }

    let (inspect_label, inspect_snippet) = if let Some(spec_rel) = spec_file {
        let spec_lean = read_generated_lean(&out_dir, spec_rel);
        let action_name = match mode {
            LeanLRuleContractMode::InvariantSendWarning { action_name, .. } => *action_name,
            _ => panic!("spec_file set but mode is not InvariantSendWarning"),
        };
        (
            format!("invariant step `.{action_name}` in `{spec_rel}`"),
            extract_spec_step_snippet(&spec_lean, action_name),
        )
    } else {
        let routes_lean = read_generated_lean(&out_dir, routes_file);
        (
            format!("route `{route_name}`"),
            extract_route_def_snippet(&routes_lean, route_name),
        )
    };

    let lake = if lean_build_enabled() {
        let start = std::time::Instant::now();
        let lake = run_lake(&out_dir);
        assert!(
            start.elapsed() <= LAKE_BUILD_TIMEOUT,
            "lake build exceeded {}s wall clock",
            LAKE_BUILD_TIMEOUT.as_secs()
        );
        lake
    } else {
        LakeRun {
            ran: false,
            ok: false,
            combined: "skipped (set CAMBRIAN_TEST_LEAN_BUILD=1 to enable)".into(),
        }
    };

    let run = evaluate_lean_l_rule_case(case, &diags, &inspect_label, &inspect_snippet, &lake);
    let _ = std::fs::remove_dir_all(&out_dir);
    run
}

fn run_validator_case(case: &ValidatorAuditCase) -> ValidatorRun {
    let yaml_path = project_yaml_path(case.project_yaml);
    let project = match Project::load(&yaml_path) {
        Ok(p) => p,
        Err(err) => {
            return ValidatorRun {
                ok: false,
                confirmed: false,
                detail: format!("load project: {err}"),
            };
        }
    };

    match &case.kind {
        ValidatorAuditKind::L13StdCryptoReject { .. } => run_lean_l13_std_crypto_case(case, &project),
        ValidatorAuditKind::LeanLRuleContract { .. } => run_lean_l_rule_case(case, &project),
        _ => run_evm_validator_case(case, &project),
    }
}

fn case_needs_forge(case: &ValidatorAuditCase) -> bool {
    case.forge_test_file.is_some()
}

#[test]
fn audit_validator_hypotheses_report() {
    if CASES.iter().any(case_needs_forge) && !has_forge() {
        eprintln!(
            "skipping audit_validator_hypotheses_report: forge not on PATH \
             (coverage / non-Foundry CI images; Forge execution lives in \
             test-transpiler-audit-validator)"
        );
        return;
    }
    let mut confirmed = Vec::new();
    let mut report = Vec::new();
    for case in CASES {
        let run = run_validator_case(case);
        let status = if run.ok {
            if run.confirmed {
                "DISPROVEN"
            } else {
                "ALIGNED / DISPROVEN"
            }
        } else if run.confirmed {
            "CONFIRMED"
        } else {
            "INCONCLUSIVE"
        };
        report.push(format!(
            "{} / {}: {}\n{}",
            case.id, case.hypothesis, status, run.detail
        ));
        if run.confirmed {
            confirmed.push(format!(
                "{} {}: CONFIRMED\n{}",
                case.id, case.hypothesis, run.detail
            ));
        }
    }
    eprintln!("=== Validator audit report ===\n{}", report.join("\n---\n"));
    if !confirmed.is_empty() {
        panic!(
            "Validator audit: {} confirmed case(s):\n\n{}",
            confirmed.len(),
            confirmed.join("\n---\n")
        );
    }
}

#[test]
fn audit_val_001_e07_filter_map_fold() {
    if !has_forge() {
        eprintln!("skipping: forge not on PATH");
        return;
    }
    let case = &CASES[0];
    let run = run_validator_case(case);
    assert!(
        run.ok,
        "{} {} failed (confirmed or inconclusive):\n{}",
        case.id,
        case.hypothesis,
        run.detail
    );
}

#[test]
fn audit_val_002_e23_mixed_tuple_purefn() {
    if !has_forge() {
        eprintln!("skipping: forge not on PATH");
        return;
    }
    let case = &CASES[1];
    let run = run_validator_case(case);
    assert!(
        run.ok,
        "{} {} failed (confirmed or inconclusive):\n{}",
        case.id,
        case.hypothesis,
        run.detail
    );
}

#[test]
fn audit_val_003_l11_failing_call_lean() {
    let case = CASES
        .iter()
        .find(|c| c.id == "T-VAL-003")
        .expect("T-VAL-003 case");
    let run = run_validator_case(case);
    eprintln!(
        "{} {}:\n{}",
        case.id, case.hypothesis, run.detail
    );
    if lean_build_enabled() && !has_lake() {
        panic!("CAMBRIAN_TEST_LEAN_BUILD=1 set but `lake` not on PATH");
    }
    assert!(
        run.ok,
        "{} {} failed (confirmed or inconclusive):\n{}",
        case.id,
        case.hypothesis,
        run.detail
    );
}

#[test]
fn audit_val_003b_l8_unresolved_send_lean() {
    let case = CASES
        .iter()
        .find(|c| c.id == "T-VAL-003b-L8")
        .expect("T-VAL-003b-L8 case");
    let run = run_validator_case(case);
    eprintln!(
        "{} {}:\n{}",
        case.id, case.hypothesis, run.detail
    );
    if lean_build_enabled() && !has_lake() {
        panic!("CAMBRIAN_TEST_LEAN_BUILD=1 set but `lake` not on PATH");
    }
    assert!(
        run.ok,
        "{} {} failed (confirmed or inconclusive):\n{}",
        case.id,
        case.hypothesis,
        run.detail
    );
}

#[test]
fn audit_val_003b_l9_failing_capture_lean() {
    let case = CASES
        .iter()
        .find(|c| c.id == "T-VAL-003b-L9")
        .expect("T-VAL-003b-L9 case");
    let run = run_validator_case(case);
    eprintln!(
        "{} {}:\n{}",
        case.id, case.hypothesis, run.detail
    );
    if lean_build_enabled() && !has_lake() {
        panic!("CAMBRIAN_TEST_LEAN_BUILD=1 set but `lake` not on PATH");
    }
    assert!(
        run.ok,
        "{} {} failed (confirmed or inconclusive):\n{}",
        case.id,
        case.hypothesis,
        run.detail
    );
}

#[test]
fn audit_val_003b_l10_invariant_send_lean() {
    let case = CASES
        .iter()
        .find(|c| c.id == "T-VAL-003b-L10")
        .expect("T-VAL-003b-L10 case");
    let run = run_validator_case(case);
    eprintln!(
        "{} {}:\n{}",
        case.id, case.hypothesis, run.detail
    );
    if lean_build_enabled() && !has_lake() {
        panic!("CAMBRIAN_TEST_LEAN_BUILD=1 set but `lake` not on PATH");
    }
    assert!(
        run.ok,
        "{} {} failed (confirmed or inconclusive):\n{}",
        case.id,
        case.hypothesis,
        run.detail
    );
}

#[test]
fn audit_val_e26_rescue_rejected_on_evm_domain() {
    let project = Project::load(&project_yaml_path("val_l12_rescue_lean.yaml"))
        .expect("load val_l12_rescue_lean.yaml");
    for target in [Target::Evm, Target::Lean] {
        let diags = check_target_compat(&project.merged, target, false);
        assert!(
            diags.iter().any(|d| d.code == "E26" && d.severity == Severity::Error),
            "T-VAL-003b-L12 superseded: E26 must reject rescue on {}: {:?}",
            target.name(),
            diags
        );
        assert!(
            !diags.iter().any(|d| d.code == "L12"),
            "L12 must not emit (superseded by E26) on {}: {:?}",
            target.name(),
            diags
        );
    }
}

#[test]
fn audit_val_004_e15_gosh_expr_reject() {
    let case = CASES
        .iter()
        .find(|c| c.id == "T-VAL-004")
        .expect("T-VAL-004 case");
    let run = run_validator_case(case);
    eprintln!(
        "{} {}:\n{}",
        case.id, case.hypothesis, run.detail
    );
    assert!(
        run.ok,
        "{} {} failed (confirmed or inconclusive):\n{}",
        case.id,
        case.hypothesis,
        run.detail
    );
}

#[test]
fn audit_val_005_e16_address_of_reject() {
    let case = CASES
        .iter()
        .find(|c| c.id == "T-VAL-005")
        .expect("T-VAL-005 case");
    let run = run_validator_case(case);
    eprintln!(
        "{} {}:\n{}",
        case.id, case.hypothesis, run.detail
    );
    assert!(
        run.ok,
        "{} {} failed (confirmed or inconclusive):\n{}",
        case.id,
        case.hypothesis,
        run.detail
    );
}

#[test]
fn audit_val_006_e17_hashmap_transform_reject() {
    let case = CASES
        .iter()
        .find(|c| c.id == "T-VAL-006")
        .expect("T-VAL-006 case");
    let run = run_validator_case(case);
    eprintln!(
        "{} {}:\n{}",
        case.id, case.hypothesis, run.detail
    );
    assert!(
        run.ok,
        "{} {} failed (confirmed or inconclusive):\n{}",
        case.id,
        case.hypothesis,
        run.detail
    );
}

#[test]
fn audit_val_007_e18_tuple_type_reject() {
    let case = CASES
        .iter()
        .find(|c| c.id == "T-VAL-007")
        .expect("T-VAL-007 case");
    let run = run_validator_case(case);
    eprintln!(
        "{} {}:\n{}",
        case.id, case.hypothesis, run.detail
    );
    assert!(
        run.ok,
        "{} {} failed (confirmed or inconclusive):\n{}",
        case.id,
        case.hypothesis,
        run.detail
    );
}

#[test]
fn audit_val_008_e19_unknown_generic_reject() {
    let case = CASES
        .iter()
        .find(|c| c.id == "T-VAL-008")
        .expect("T-VAL-008 case");
    let run = run_validator_case(case);
    eprintln!(
        "{} {}:\n{}",
        case.id, case.hypothesis, run.detail
    );
    assert!(
        run.ok,
        "{} {} failed (confirmed or inconclusive):\n{}",
        case.id,
        case.hypothesis,
        run.detail
    );
}

#[test]
fn audit_val_009_e20_unknown_simple_reject() {
    let case = CASES
        .iter()
        .find(|c| c.id == "T-VAL-009")
        .expect("T-VAL-009 case");
    let run = run_validator_case(case);
    eprintln!(
        "{} {}:\n{}",
        case.id, case.hypothesis, run.detail
    );
    assert!(
        run.ok,
        "{} {} failed (confirmed or inconclusive):\n{}",
        case.id,
        case.hypothesis,
        run.detail
    );
}

#[test]
fn audit_val_010_e21_invalid_cast_reject() {
    let case = CASES
        .iter()
        .find(|c| c.id == "T-VAL-010")
        .expect("T-VAL-010 case");
    let run = run_validator_case(case);
    eprintln!(
        "{} {}:\n{}",
        case.id, case.hypothesis, run.detail
    );
    assert!(
        run.ok,
        "{} {} failed (confirmed or inconclusive):\n{}",
        case.id,
        case.hypothesis,
        run.detail
    );
}

fn write_std_val_repro(subdir: &str, yaml_rel: &str, cam_rel: &str, title: &str, detail: &str) {
    let dir = audit_root().join("validator_repro").join(subdir);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create repro dir");
    let yaml_src = project_yaml_path(yaml_rel);
    let cam_src = audit_root().join(cam_rel);
    if yaml_src.exists() {
        let name = yaml_src.file_name().unwrap();
        let _ = fs::copy(&yaml_src, dir.join(name));
    }
    if cam_src.exists() {
        let name = cam_src.file_name().unwrap();
        let _ = fs::copy(&cam_src, dir.join(name));
    }
    fs::write(
        dir.join("NOTE.md"),
        format!("# {title}\n\n## Detail\n\n{detail}\n"),
    )
    .expect("write NOTE.md");
}

#[test]
fn audit_std_val_001() {
    let case = CASES
        .iter()
        .find(|c| c.id == "T-STD-VAL-001")
        .expect("T-STD-VAL-001 case");
    let run = run_validator_case(case);
    eprintln!(
        "{} {}:\n{}",
        case.id, case.hypothesis, run.detail
    );
    if !run.ok {
        write_std_val_repro(
            "std_val_001",
            "validator/std_val_001_bare_min.yaml",
            "validator/std_val_001_bare_min.cam",
            "T-STD-VAL-001 repro",
            &run.detail,
        );
    }
    assert!(
        run.ok,
        "{} {} failed (confirmed or inconclusive):\n{}",
        case.id,
        case.hypothesis,
        run.detail
    );
}

#[test]
fn audit_std_val_002() {
    let case = CASES
        .iter()
        .find(|c| c.id == "T-STD-VAL-002")
        .expect("T-STD-VAL-002 case");
    let run = run_validator_case(case);
    eprintln!(
        "{} {}:\n{}",
        case.id, case.hypothesis, run.detail
    );
    if !run.ok {
        write_std_val_repro(
            "std_val_002",
            "validator/std_val_002_string_to_u64.yaml",
            "validator/std_val_002_string_to_u64.cam",
            "T-STD-VAL-002 repro",
            &run.detail,
        );
    }
    assert!(
        run.ok,
        "{} {} failed (confirmed or inconclusive):\n{}",
        case.id,
        case.hypothesis,
        run.detail
    );
}

#[test]
fn audit_std_val_003() {
    let case = CASES
        .iter()
        .find(|c| c.id == "T-STD-VAL-003")
        .expect("T-STD-VAL-003 case");
    let run = run_validator_case(case);
    eprintln!(
        "{} {}:\n{}",
        case.id, case.hypothesis, run.detail
    );
    if !run.ok {
        write_std_val_repro(
            "std_val_003",
            "validator/std_val_003_bare_sha256.yaml",
            "validator/std_val_003_bare_sha256.cam",
            "T-STD-VAL-003 repro",
            &run.detail,
        );
    }
    assert!(
        run.ok,
        "{} {} failed (confirmed or inconclusive):\n{}",
        case.id,
        case.hypothesis,
        run.detail
    );
}

#[test]
fn audit_std_val_004() {
    let case = CASES
        .iter()
        .find(|c| c.id == "T-STD-VAL-004")
        .expect("T-STD-VAL-004 case");
    let run = run_validator_case(case);
    eprintln!(
        "{} {}:\n{}",
        case.id, case.hypothesis, run.detail
    );
    if !run.ok {
        write_std_val_repro(
            "std_val_004",
            "validator/std_val_004_std_crypto_lean.yaml",
            "validator/std_val_004_std_crypto_lean.cam",
            "T-STD-VAL-004 repro",
            &run.detail,
        );
    }
    assert!(
        run.ok,
        "{} {} failed (confirmed or inconclusive):\n{}",
        case.id,
        case.hypothesis,
        run.detail
    );
}
