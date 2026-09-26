// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! EVM compile regression via **Forge** (`forge build`), not direct `solc`.
//!
//! Transpiles `.cam` fixtures and asserts the generated Foundry project
//! compiles. See [docs/AUDIT_EVM_LEAN.md](../../docs/AUDIT_EVM_LEAN.md).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use cambrian_transpiler::target::Domain;

#[path = "corpus/mod.rs"]
mod corpus;

// Each helper invocation gets a unique suffix so concurrent tests that
// happen to operate on the same fixture (e.g. the dedicated
// `evm_target_counter_compiles_with_solc` and the bulk
// `evm10_all_fixtures_compile_with_solc` both touching `counter.cam`)
// don't race on `remove_dir_all` / `create_dir_all` of the same path.
static OUT_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_out_dir(prefix: &str, stem: &str) -> PathBuf {
    let n = OUT_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cambrian-evm-{}-{}-{}-{}",
        prefix,
        stem,
        std::process::id(),
        n
    ))
}

fn transpiler_bin() -> PathBuf {
    // `CARGO_BIN_EXE_*` rather than a walk up from `current_exe()`: the two
    // are the same path under the classic `target/debug/deps` layout, but a
    // cargo configured with a split build directory puts the test executable
    // somewhere else entirely, and the walk then points at a file that does
    // not exist. The failure reads as "No such file or directory" from the
    // spawn, which names neither the binary nor the layout.
    PathBuf::from(env!("CARGO_BIN_EXE_cambrian-transpiler"))
}

const FOUNDRY_TOML: &str = r#"[profile.default]
src = "src"
out = "out"
libs = ["lib"]
solc_version = "0.8.24"
auto_detect_solc = false
evm_version = "cancun"
optimizer = false
optimizer_runs = 200
via_ir = false

[lint]
lint_on_build = false
"#;

fn has_forge() -> bool {
    Command::new("forge")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run_transpiler(input: &Path, out_dir: &Path) -> std::process::Output {
    Command::new(transpiler_bin())
        .arg(input)
        .arg("-o")
        .arg(out_dir)
        .arg("--target")
        .arg("evm")
        .output()
        .expect("failed to run transpiler")
}

/// Prefer a compact, actionable forge failure line (stderr first). The old
/// reporter took `err.lines().next()`, which was always the literal
/// `"stdout:"` prefix and hid the real diagnostic.
fn format_forge_failure(stdout: &str, stderr: &str) -> String {
    let pick = |s: &str| {
        s.lines()
            .map(str::trim)
            .find(|l| {
                !l.is_empty()
                    && *l != "stdout:"
                    && *l != "stderr:"
                    && !l.starts_with("Compiling ")
                    && !l.starts_with("Solc ")
            })
            .unwrap_or("")
            .to_string()
    };
    let summary = {
        let from_err = pick(stderr);
        if !from_err.is_empty() {
            from_err
        } else {
            pick(stdout)
        }
    };
    format!(
        "summary: {}\nstdout:\n{}\nstderr:\n{}",
        if summary.is_empty() {
            "(empty forge output)".to_string()
        } else {
            summary
        },
        stdout,
        stderr
    )
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn check_fixture_transpile_only(fixture: &Path) -> (bool, String) {
    let stem = fixture.file_stem().unwrap_or_default().to_string_lossy();
    let out_dir = unique_out_dir("tp", &stem);
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).expect("create temp dir");
    let tp_out = run_transpiler(fixture, &out_dir);
    let ok = tp_out.status.success();
    let err = String::from_utf8_lossy(&tp_out.stderr).to_string();
    let _ = std::fs::remove_dir_all(&out_dir);
    (ok, err)
}

fn check_fixture_with_forge(fixture: &Path) -> (bool, bool, String) {
    let stem = fixture.file_stem().unwrap_or_default().to_string_lossy();
    let out_dir = unique_out_dir("forge", &stem);
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).expect("create temp dir");

    let tp_out = run_transpiler(fixture, &out_dir);
    if !tp_out.status.success() {
        let err = String::from_utf8_lossy(&tp_out.stderr).to_string();
        let _ = std::fs::remove_dir_all(&out_dir);
        return (false, false, err);
    }

    // Transpiler writes `src/<Entity>.sol`; Forge needs a project root.
    let src_dir = out_dir.join("src");
    if !src_dir.is_dir() {
        let _ = std::fs::remove_dir_all(&out_dir);
        return (false, false, "no src/ directory in transpiler output".to_string());
    }

    let sol_count = std::fs::read_dir(&src_dir)
        .ok()
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.path().extension().map_or(false, |ext| ext == "sol"))
                .count()
        })
        .unwrap_or(0);
    if sol_count == 0 {
        let _ = std::fs::remove_dir_all(&out_dir);
        return (false, false, "no .sol file generated under src/".to_string());
    }

    std::fs::write(out_dir.join("foundry.toml"), FOUNDRY_TOML).expect("write foundry.toml");

    let forge = Command::new("forge")
        .args(["build", "--root"])
        .arg(&out_dir)
        .args(["--skip", ".t.sol", "--use", "0.8.24"])
        .output()
        .expect("run forge build");

    let ok = forge.status.success();
    let err = if ok {
        String::new()
    } else {
        format_forge_failure(
            &String::from_utf8_lossy(&forge.stdout),
            &String::from_utf8_lossy(&forge.stderr),
        )
    };
    let _ = std::fs::remove_dir_all(&out_dir);
    (true, ok, err)
}

// ---------------------------------------------------------------------------
// Existing integration tests
// ---------------------------------------------------------------------------

#[test]
fn evm_target_counter_compiles_with_solc() {
    if !has_forge() {
        eprintln!("skipping: forge not found in PATH");
        return;
    }

    let (tp_ok, solc_ok, err) = check_fixture_with_forge(Path::new("../contracts/counter.cam"));
    assert!(tp_ok, "transpiler failed for counter");
    assert!(solc_ok, "forge failed for counter:\n{}", err);
}

#[test]
fn evm6_m1_extern_entity_compiles_with_solc() {
    if !has_forge() {
        eprintln!("skipping: forge not found in PATH");
        return;
    }

    let (tp_ok, solc_ok, err) = check_fixture_with_forge(
        Path::new("../contracts/extern_token_caller.cam"),
    );
    assert!(tp_ok, "transpiler failed for extern_token_caller");
    assert!(solc_ok, "forge failed for extern_token_caller:\n{}", err);
}

#[test]
fn evm_target_complex_route_compiles_with_solc() {
    if !has_forge() {
        eprintln!("skipping: forge not found in PATH");
        return;
    }

    let out_dir = std::env::temp_dir().join("cambrian-evm-complex");
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).expect("create temp out dir");

    let cam_source = r#"
extern entity Receiver {
    accept route notify(amount: u64);
}

entity Messenger {
    routes {
        authorize(owner: address, admin: address, to: Address<Receiver>, amount: u64)
            from ownerEntity(owner) | adminEntity(admin)
            where amount > 0 : throw 42
            => [
                notify(amount) ~> to with { value: amount }
                ~> to with { value: amount }
            ]
    }
    m_count: u64 {
        in authorize(owner, admin, to, amount) => m_count + amount
    }
}
"#;
    let cam_file = out_dir.join("messenger.cam");
    std::fs::write(&cam_file, cam_source).expect("write .cam fixture");

    let output = run_transpiler(&cam_file, &out_dir);
    assert!(
        output.status.success(),
        "transpiler failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    std::fs::write(out_dir.join("foundry.toml"), FOUNDRY_TOML).expect("write foundry.toml");

    let forge = Command::new("forge")
        .args(["build", "--root"])
        .arg(&out_dir)
        .args(["--use", "0.8.24"])
        .output()
        .expect("failed to run forge build");
    assert!(
        forge.status.success(),
        "forge build failed:\n{}",
        format_forge_failure(
            &String::from_utf8_lossy(&forge.stdout),
            &String::from_utf8_lossy(&forge.stderr),
        )
    );

    let _ = std::fs::remove_dir_all(&out_dir);
}

// ---------------------------------------------------------------------------
// P8: regression suite over the shared EVM-domain corpus
// ---------------------------------------------------------------------------

/// Fixtures that transpile to EVM Solidity but produce code that
/// `solc` rejects today. Keys are corpus keys (`counter`,
/// `marketplace/ledger`). Graduation panic when an entry starts compiling.
const ALL_FIXTURES_SOLC_KNOWN_BROKEN: &[(&str, &str)] = &[];

#[test]
fn evm10_all_fixtures_transpile_successfully() {
    let fixtures = corpus::domain_fixtures(Domain::Evm);
    assert!(
        !fixtures.is_empty(),
        "EVM-domain corpus is empty — check corpus::domain_fixtures"
    );
    let mut failures: Vec<(String, String)> = vec![];

    for fx in &fixtures {
        assert!(
            fx.path.exists(),
            "corpus fixture missing on disk: {} ({})",
            fx.key,
            fx.path.display()
        );
        let (ok, err) = check_fixture_transpile_only(&fx.path);
        if !ok {
            failures.push((fx.key.clone(), err));
        }
    }

    if !failures.is_empty() {
        let msgs: Vec<String> = failures
            .iter()
            .map(|(n, e)| format!("  [{}]: {}", n, e.lines().next().unwrap_or("error")))
            .collect();
        panic!(
            "{} fixture(s) failed to transpile:\n{}",
            failures.len(),
            msgs.join("\n")
        );
    }
}

#[test]
fn evm10_all_fixtures_compile_with_solc() {
    if !has_forge() {
        eprintln!("skipping: forge not found in PATH");
        return;
    }

    let fixtures = corpus::domain_fixtures(Domain::Evm);
    let mut failures: Vec<(String, String)> = vec![];

    let known_broken: std::collections::HashSet<&str> = ALL_FIXTURES_SOLC_KNOWN_BROKEN
        .iter()
        .map(|(n, _)| *n)
        .collect();
    let mut graduated: Vec<String> = vec![];
    for fx in &fixtures {
        assert!(
            fx.path.exists(),
            "corpus fixture missing on disk: {} ({})",
            fx.key,
            fx.path.display()
        );
        let is_known_broken = known_broken.contains(fx.key.as_str());
        let (tp_ok, solc_ok, err) = check_fixture_with_forge(&fx.path);
        if is_known_broken {
            if tp_ok && solc_ok {
                graduated.push(fx.key.clone());
            }
            continue;
        }
        if !tp_ok {
            failures.push((format!("{} [transpiler]", fx.key), err));
        } else if !solc_ok {
            failures.push((format!("{} [solc]", fx.key), err));
        }
    }

    if !graduated.is_empty() {
        panic!(
            "{} fixture(s) now compile cleanly with solc — remove them from `ALL_FIXTURES_SOLC_KNOWN_BROKEN`:\n  {}",
            graduated.len(),
            graduated.join("\n  ")
        );
    }

    if !failures.is_empty() {
        let msgs: Vec<String> = failures
            .iter()
            .map(|(n, e)| {
                // Prefer the `summary:` line produced by `format_forge_failure`;
                // fall back to the first non-empty diagnostic line.
                let summary = e
                    .lines()
                    .find_map(|l| l.strip_prefix("summary: "))
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .or_else(|| {
                        e.lines()
                            .map(str::trim)
                            .find(|l| {
                                !l.is_empty()
                                    && *l != "stdout:"
                                    && *l != "stderr:"
                                    && !l.starts_with("Compiling ")
                            })
                    })
                    .unwrap_or("error");
                format!("  [{n}]: {summary}")
            })
            .collect();
        panic!(
            "{} fixture(s) failed:\n{}\n\n(full forge logs above per failure `summary`)",
            failures.len(),
            msgs.join("\n")
        );
    }
}

/// Sanity check: every EVM-domain [`corpus::REJECTED_ON`] entry must hit
/// the documented validator code when transpiled for `--target evm`.
/// Graduation panic when an entry starts accepting.
#[test]
fn evm10_incompatible_fixtures_fail_validation() {
    let by_key: std::collections::BTreeMap<_, _> = corpus::contracts_primaries()
        .into_iter()
        .map(|f| (f.key.clone(), f))
        .collect();
    let mut bad: Vec<String> = vec![];
    for (key, domain, expected_code, _reason) in corpus::REJECTED_ON {
        if *domain != Domain::Evm {
            continue;
        }
        let Some(fx) = by_key.get(*key) else {
            bad.push(format!(
                "{key}: fixture file missing (drop the entry from REJECTED_ON)"
            ));
            continue;
        };
        let (ok, err) = check_fixture_transpile_only(&fx.path);
        if ok {
            bad.push(format!(
                "{key}: now transpiles cleanly to EVM — remove from REJECTED_ON (expected error code {expected_code})"
            ));
            continue;
        }
        if !err.contains(&format!("[{}]", expected_code)) {
            bad.push(format!(
                "{key}: failed for a different reason than {expected_code} — update the expected code or fix the fixture\n--- transpiler output ---\n{err}"
            ));
        }
    }
    if !bad.is_empty() {
        panic!(
            "{} EVM-incompatible fixture issue(s):\n  {}",
            bad.len(),
            bad.join("\n  ")
        );
    }
}

// ---------------------------------------------------------------------------
// EVM-11: cross-contract view call — dedicated solc test
// ---------------------------------------------------------------------------

#[test]
fn evm11_token_query_fixture_transpiles() {
    let (ok, err) = check_fixture_transpile_only(Path::new("../contracts/token_query.cam"));
    assert!(ok, "token_query transpiler failed:\n{}", err);
}

#[test]
fn evm11_token_query_fixture_compiles_with_solc() {
    if !has_forge() {
        eprintln!("skipping: forge not found in PATH");
        return;
    }
    let (tp_ok, solc_ok, err) = check_fixture_with_forge(Path::new("../contracts/token_query.cam"));
    assert!(tp_ok, "token_query transpiler failed:\n{}", err);
    assert!(solc_ok, "token_query forge failed:\n{}", err);
}

// ---------------------------------------------------------------------------
// EVM `for` loop lowering — dedicated transpile + solc + shape test.
//
// Verifies that:
//   1. `loop_evm.cam` transpiles cleanly with no `E07` warnings.
//   2. The generated Solidity contains both lowered loop shapes
//      (Range and Vec<T>) — i.e. real `for (uint256 _i = 0; _i < ...; ++_i) { ... }`
//      blocks, not `revert("EVM: functional iterator not supported")`.
//   3. The generated Solidity actually compiles with solc.
// ---------------------------------------------------------------------------

#[test]
fn evm_for_loop_fixture_transpiles_without_e07() {
    let (ok, err) = check_fixture_transpile_only(Path::new("../contracts/loop_evm.cam"));
    assert!(ok, "loop_evm transpiler failed:\n{}", err);
    assert!(
        !err.contains("[E07]"),
        "loop_evm should not emit any E07 warnings (got:\n{})",
        err
    );
}

#[test]
fn evm_for_loop_fixture_emits_solidity_for_loops() {
    let out_dir = std::env::temp_dir().join("cambrian-evm-for-loop-shape");
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).expect("create temp dir");

    let tp_out = run_transpiler(Path::new("../contracts/loop_evm.cam"), &out_dir);
    assert!(
        tp_out.status.success(),
        "transpiler failed:\n{}",
        String::from_utf8_lossy(&tp_out.stderr)
    );

    let sol_path = [out_dir.join("src"), out_dir.clone()]
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flat_map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()))
        .find(|p| p.extension().map_or(false, |ext| ext == "sol"))
        .expect("at least one .sol file");
    let body = std::fs::read_to_string(&sol_path).expect("read sol");

    assert!(
        body.contains("for (uint256 "),
        "expected at least one Solidity `for (uint256 ` loop in:\n{}",
        body
    );
    assert!(
        !body.contains("EVM: functional iterator not supported"),
        "expected no fallback revert stub, got:\n{}",
        body
    );
    // Range form: counter initialised from a temp + index.
    // Phase EVM-P0-B: when the range's element type is narrow
    // (e.g. `0..n` over `n: u32`), the index gets explicitly cast
    // to the narrow type, producing `_cam_tmpN + uint32(_cam_tmpM)`
    // rather than the bare `+ _cam_tmpN` shape.
    assert!(
        body.contains(" + _cam_tmp") || body.contains(" + uint32(_cam_tmp"),
        "expected range-form loop binding `start + _cam_tmpN` (or narrowed form) in:\n{}",
        body
    );
    // Vec<T> form: indexed access into a Vec parameter.
    assert!(
        body.contains("xs[_cam_tmp"),
        "expected Vec<T>-form loop binding `xs[_cam_tmpN]` in:\n{}",
        body
    );

    let _ = std::fs::remove_dir_all(&out_dir);
}

#[test]
fn evm_for_loop_fixture_compiles_with_solc() {
    if !has_forge() {
        eprintln!("skipping: forge not found in PATH");
        return;
    }
    let (tp_ok, solc_ok, err) =
        check_fixture_with_forge(Path::new("../contracts/loop_evm.cam"));
    assert!(tp_ok, "loop_evm transpiler failed:\n{}", err);
    assert!(solc_ok, "loop_evm forge failed:\n{}", err);
}

// ---------------------------------------------------------------------------
// EVM `.fold(init, |acc, x| body)` lowering — dedicated transpile +
// solc + shape test (sibling to the `for`-loop variant above).
// ---------------------------------------------------------------------------

#[test]
fn evm_fold_fixture_transpiles_without_e07() {
    let (ok, err) = check_fixture_transpile_only(Path::new("../contracts/fold_evm.cam"));
    assert!(ok, "fold_evm transpiler failed:\n{}", err);
    assert!(
        !err.contains("[E07]"),
        "fold_evm should not emit any E07 warnings (got:\n{})",
        err
    );
}

#[test]
fn evm_fold_fixture_emits_imperative_loops() {
    let out_dir = std::env::temp_dir().join("cambrian-evm-fold-shape");
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).expect("create temp dir");

    let tp_out = run_transpiler(Path::new("../contracts/fold_evm.cam"), &out_dir);
    assert!(
        tp_out.status.success(),
        "transpiler failed:\n{}",
        String::from_utf8_lossy(&tp_out.stderr)
    );

    let sol_path = [out_dir.join("src"), out_dir.clone()]
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flat_map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()))
        .find(|p| p.extension().map_or(false, |ext| ext == "sol"))
        .expect("at least one .sol file");
    let body = std::fs::read_to_string(&sol_path).expect("read sol");

    assert!(
        body.contains("for (uint256 "),
        "expected at least one lowered fold for-loop in:\n{}",
        body
    );
    assert!(
        body.contains("uint256 r = x;"),
        "expected sqrt accumulator init `uint256 r = x;` in:\n{}",
        body
    );
    assert!(
        body.contains("uint256 acc = 0;"),
        "expected scalar fold accumulator init `uint256 acc = 0;` in:\n{}",
        body
    );
    assert!(
        !body.contains("EVM: closure-as-value"),
        "supported `.fold` shape must not fall back to the closure-as-value revert:\n{}",
        body
    );

    let _ = std::fs::remove_dir_all(&out_dir);
}

#[test]
fn evm_fold_fixture_compiles_with_solc() {
    if !has_forge() {
        eprintln!("skipping: forge not found in PATH");
        return;
    }
    let (tp_ok, solc_ok, err) =
        check_fixture_with_forge(Path::new("../contracts/fold_evm.cam"));
    assert!(tp_ok, "fold_evm transpiler failed:\n{}", err);
    assert!(solc_ok, "fold_evm forge failed:\n{}", err);
}

// ---------------------------------------------------------------------------
// EVM-13: iterable HashMap end-to-end — transpile + solc.
// ---------------------------------------------------------------------------
//
// This pins the parallel-array maintenance code paths (push on first
// insert, swap-pop on remove, `K[] m_keys` storage emission) against
// the real Solidity compiler. Unit tests in `test_codegen_evm.rs` cover
// the generated text shape; this test is the gate that says "and the
// resulting Solidity actually compiles".

#[test]
fn evm_keys_fixture_transpiles_without_e07() {
    let (ok, err) = check_fixture_transpile_only(Path::new("../contracts/keys_evm.cam"));
    assert!(ok, "keys_evm transpiler failed:\n{}", err);
    assert!(
        !err.contains("[E07]"),
        "keys_evm should not emit any E07 warnings (got:\n{})",
        err
    );
}

#[test]
fn evm_keys_fixture_compiles_with_solc() {
    if !has_forge() {
        eprintln!("skipping: forge not found in PATH");
        return;
    }
    let (tp_ok, solc_ok, err) =
        check_fixture_with_forge(Path::new("../contracts/keys_evm.cam"));
    assert!(tp_ok, "keys_evm transpiler failed:\n{}", err);
    assert!(solc_ok, "keys_evm forge failed:\n{}", err);
}

// ---------------------------------------------------------------------------
// EVM-12 Batch E: action-level `for` lowering — fixture round-trips
// through transpile + solc to confirm the loop body wraps real
// external calls and that the route signature exposes the Vec<address>
// route parameter as `address[] memory`.
// ---------------------------------------------------------------------------

#[test]
fn evm_action_for_fixture_transpiles() {
    let (ok, err) = check_fixture_transpile_only(Path::new("../contracts/airdrop_evm.cam"));
    assert!(ok, "airdrop_evm transpiler failed:\n{}", err);
}

#[test]
fn evm_action_for_fixture_emits_solidity_for_loop_with_send() {
    let out_dir = std::env::temp_dir().join("cambrian-evm-action-for-shape");
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).expect("create temp dir");

    let tp_out = run_transpiler(Path::new("../contracts/airdrop_evm.cam"), &out_dir);
    assert!(
        tp_out.status.success(),
        "transpiler failed:\n{}",
        String::from_utf8_lossy(&tp_out.stderr)
    );

    let sol_path = [out_dir.join("src"), out_dir.clone()]
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flat_map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()))
        .find(|p| p.extension().map_or(false, |ext| ext == "sol"))
        .expect("at least one .sol file");
    let body = std::fs::read_to_string(&sol_path).expect("read sol");

    assert!(
        body.contains("address[] memory recipients"),
        "Vec<address> route param must lower to address[] memory:\n{}",
        body
    );
    assert!(
        body.contains("for (uint256 "),
        "action-level for must emit a Solidity for loop:\n{}",
        body
    );
    assert!(
        body.contains("address r = recipients["),
        "loop variable must bind from the Vec<address> route param:\n{}",
        body
    );
    assert!(
        body.contains(".call{value: amount}"),
        "send body must run per iteration with the captured `value`:\n{}",
        body
    );

    let _ = std::fs::remove_dir_all(&out_dir);
}

#[test]
fn evm_action_for_fixture_compiles_with_solc() {
    if !has_forge() {
        eprintln!("skipping: forge not found in PATH");
        return;
    }
    let (tp_ok, solc_ok, err) =
        check_fixture_with_forge(Path::new("../contracts/airdrop_evm.cam"));
    assert!(tp_ok, "airdrop_evm transpiler failed:\n{}", err);
    assert!(solc_ok, "airdrop_evm forge failed:\n{}", err);
}

// ---------------------------------------------------------------------------
// EVM-13 Batch F: iterable-HashMap ordering guarantees.
//
// The contract is documented under PLAN_EVM-13 and EVM_GAPS § 2.8.7:
//
//   1. **Insert order is preserved while keys remain unique.** The
//      maintenance pushes a new key at the tail of `<m>_keys`; no
//      reordering of existing entries.
//   2. **Remove uses swap-pop.** The last key fills the removed slot;
//      the relocated key's reverse-index entry is rewritten so future
//      removes against it are O(1).
//   3. **Re-inserting a removed key appends.** Because `<m>_exists[k]`
//      is cleared on remove, the next insert of `k` runs the
//      append-and-mark path again.
//
// Rather than spinning up a full revm execution environment, we pin the
// invariants by inspecting the generated Solidity directly: the ordering
// guarantees follow mechanically from the maintenance code we emit.
// The companion `evm_keys_fixture_compiles_with_solc` test still gates
// the full solc round-trip on the same fixture, so we know the emitted
// Solidity is well-formed.
// ---------------------------------------------------------------------------

#[test]
fn evm13_insert_preserves_order_via_tail_push() {
    let out_dir = std::env::temp_dir().join("cambrian-evm-keys-shape-insert");
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).expect("create temp dir");

    let tp_out = run_transpiler(Path::new("../contracts/keys_evm.cam"), &out_dir);
    assert!(tp_out.status.success(),
        "transpiler failed:\n{}", String::from_utf8_lossy(&tp_out.stderr));

    let sol_path = [out_dir.join("src"), out_dir.clone()]
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flat_map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()))
        .find(|p| p.extension().map_or(false, |ext| ext == "sol"))
        .expect("sol file");
    let body = std::fs::read_to_string(&sol_path).expect("read sol");

    assert!(
        body.contains("if (!m_balances_exists["),
        "insert must guard the append on the existence flag:\n{}",
        body
    );
    assert!(
        body.contains("m_balances_keys_index[") &&
            body.contains("] = m_balances_keys.length;"),
        "insert must record the index in the reverse map BEFORE the push:\n{}",
        body
    );
    assert!(
        body.contains("m_balances_keys.push("),
        "insert must append the new key at the tail of the parallel array:\n{}",
        body
    );
    assert!(
        body.contains("m_balances_exists[") && body.contains("] = true;"),
        "insert must mark the key as present so re-inserts of the same key are no-ops:\n{}",
        body
    );

    let _ = std::fs::remove_dir_all(&out_dir);
}

#[test]
fn evm13_remove_uses_swap_pop_and_clears_existence_flag() {
    let out_dir = std::env::temp_dir().join("cambrian-evm-keys-shape-remove");
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).expect("create temp dir");

    let tp_out = run_transpiler(Path::new("../contracts/keys_evm.cam"), &out_dir);
    assert!(tp_out.status.success(),
        "transpiler failed:\n{}", String::from_utf8_lossy(&tp_out.stderr));

    let sol_path = [out_dir.join("src"), out_dir.clone()]
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flat_map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()))
        .find(|p| p.extension().map_or(false, |ext| ext == "sol"))
        .expect("sol file");
    let body = std::fs::read_to_string(&sol_path).expect("read sol");

    assert!(
        body.contains("m_balances_keys.length - 1"),
        "swap-pop must read the last index of the parallel array:\n{}",
        body
    );
    assert!(
        body.contains("m_balances_keys[") &&
            body.contains("m_balances_keys.pop();"),
        "swap-pop must overwrite the removed slot with the last key and then pop():\n{}",
        body
    );
    assert!(
        body.contains("m_balances_keys_index[") &&
            body.matches("m_balances_keys_index[").count() >= 3,
        "swap-pop must rewrite the reverse-index entry of the relocated key:\n{}",
        body
    );
    assert!(
        body.contains("delete m_balances_exists["),
        "remove must clear the existence flag so re-inserts append at the tail again (Batch F invariant 3):\n{}",
        body
    );
    assert!(
        body.contains("delete m_balances_keys_index["),
        "remove must clear the reverse index for the removed key so future inserts get a fresh slot:\n{}",
        body
    );

    let _ = std::fs::remove_dir_all(&out_dir);
}

#[test]
fn evm13_reinsert_after_remove_takes_append_path() {
    // The append-vs-no-op decision is gated on `m_balances_exists[k]`.
    // Remove deletes that flag, so the next insert of the same key
    // must fall through the same "guarded append" prefix the
    // first-insert path uses. Re-running the insert assertion against
    // the same fixture is sufficient because the generated body for
    // `register` runs unconditionally — it is the existence flag that
    // determines whether the append branch fires per call.
    let out_dir = std::env::temp_dir().join("cambrian-evm-keys-shape-reinsert");
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).expect("create temp dir");

    let tp_out = run_transpiler(Path::new("../contracts/keys_evm.cam"), &out_dir);
    assert!(tp_out.status.success(),
        "transpiler failed:\n{}", String::from_utf8_lossy(&tp_out.stderr));

    let sol_path = [out_dir.join("src"), out_dir.clone()]
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flat_map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()))
        .find(|p| p.extension().map_or(false, |ext| ext == "sol"))
        .expect("sol file");
    let body = std::fs::read_to_string(&sol_path).expect("read sol");

    let push_count = body.matches("m_balances_keys.push(").count();
    assert!(
        push_count >= 1,
        "expected at least one push() call in the insert maintenance:\n{}",
        body
    );
    assert!(
        body.contains("delete m_balances_exists["),
        "re-insert path requires that remove clears the existence flag — \
         otherwise the second register() of the same key would silently \
         skip the append:\n{}",
        body
    );

    let _ = std::fs::remove_dir_all(&out_dir);
}

#[test]
fn evm_p0_c_solc_round_trip_erc20_events() {
    if !has_forge() {
        eprintln!("skipping: forge not found in PATH");
        return;
    }
    let (tp_ok, solc_ok, err) =
        check_fixture_with_forge(Path::new("../contracts/erc20_events_evm.cam"));
    assert!(tp_ok, "transpiler failed for erc20_events_evm:\n{}", err);
    assert!(solc_ok, "forge failed for erc20_events_evm:\n{}", err);
}

#[test]
fn evm_sd03_rehearsal_token_events_compile() {
    if !has_forge() {
        eprintln!("skipping: forge not found in PATH");
        return;
    }
    let (tp_ok, solc_ok, err) = check_fixture_with_forge(
        Path::new("../contracts/rehearsal_token_events_evm.cam"),
    );
    assert!(tp_ok, "transpiler failed for rehearsal_token_events_evm:\n{}", err);
    assert!(solc_ok, "forge failed for rehearsal_token_events_evm:\n{}", err);
}

#[test]
fn evm_p0_d_solc_round_trip_erc20_errors() {
    if !has_forge() {
        eprintln!("skipping: forge not found in PATH");
        return;
    }
    let (tp_ok, solc_ok, err) =
        check_fixture_with_forge(Path::new("../contracts/erc20_errors_evm.cam"));
    assert!(tp_ok, "transpiler failed for erc20_errors_evm:\n{}", err);
    assert!(solc_ok, "forge failed for erc20_errors_evm:\n{}", err);
}

#[test]
fn evm_p0_e_solc_round_trip_eth_vault() {
    if !has_forge() {
        eprintln!("skipping: forge not found in PATH");
        return;
    }
    let (tp_ok, solc_ok, err) =
        check_fixture_with_forge(Path::new("../contracts/eth_vault_evm.cam"));
    assert!(tp_ok, "transpiler failed for eth_vault_evm:\n{}", err);
    assert!(solc_ok, "forge failed for eth_vault_evm:\n{}", err);
}

#[test]
fn setup_sh_installs_forge_std_without_git_submodule() {
    let sh = cambrian_transpiler::codegen::evm_test_codegen::generate_setup_sh();
    assert!(
        sh.contains("forge install foundry-rs/forge-std --no-git"),
        "setup.sh must use --no-git so Foundry does not gitlink this repo: {sh}"
    );
    assert!(
        !sh.contains("--root"),
        "--root would make Foundry treat the Cambrian checkout as the install root: {sh}"
    );
}

const EXPECT_PREDICATE_CAM: &str = r#"
entity Counter {
    routes {
        view getCount() -> u64 => [return(m_count)]
        view pair() -> (u64, u64) => [return((m_count, m_spent))]
        bump() => []
    }
    m_count: u64 { in bump() => m_count + 1 }
    m_spent: u64 { in bump() => m_spent }
}

property "holds" for Counter {
    call bump()
    call getCount()
    expect return != 0
    expect return > 0 && m_count >= 1
    expect m_count + 0 == m_count
}

property "too_big" for Counter {
    call bump()
    call getCount()
    expect return > 100
}

property "lens_holds" for Counter {
    call bump()
    call pair()
    expect return.0 > 0
}
"#;

#[test]
fn expect_predicate_forge_test_runs() {
    if !has_forge() {
        eprintln!("skipping expect_predicate_forge_test_runs: forge not found in PATH");
        return;
    }

    let out_dir = unique_out_dir("forge", "expect-pred");
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).expect("create temp dir");
    let cam = out_dir.join("pred.cam");
    std::fs::write(&cam, EXPECT_PREDICATE_CAM).unwrap();

    let tp = run_transpiler(&cam, &out_dir);
    assert!(
        tp.status.success(),
        "transpile failed:\n{}",
        String::from_utf8_lossy(&tp.stderr)
    );
    cambrian_transpiler::codegen::evm_test_codegen::install_forge_std(&out_dir)
        .unwrap_or_else(|e| panic!("{e}"));

    let forge = Command::new("forge")
        .args(["test", "--match-contract", "CounterTest", "-vv", "--root"])
        .arg(&out_dir)
        .output()
        .expect("forge test");
    let log = format!(
        "{}\n{}",
        String::from_utf8_lossy(&forge.stdout),
        String::from_utf8_lossy(&forge.stderr)
    );
    let _ = std::fs::remove_dir_all(&out_dir);
    assert!(
        log.contains("[PASS] test_holds()"),
        "true predicate must pass forge:\n{log}"
    );
    assert!(
        log.contains("[PASS] test_lens_holds()"),
        "tuple lens predicate must pass forge:\n{log}"
    );
    assert!(
        log.contains("[FAIL") && log.contains("test_too_big"),
        "false predicate must fail forge:\n{log}"
    );
    assert!(
        log.contains("expect predicate failed"),
        "false predicate must fail inside assertTrue, not at compile time:\n{log}"
    );
}
