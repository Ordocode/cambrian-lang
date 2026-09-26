// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! P8 asymmetry gate: every multi-core domain's fixtures must agree on
//! accept/reject across cores (outside explicit [`corpus::CORE_REJECTS`]).

#[path = "corpus/mod.rs"]
mod corpus;

use cambrian_transpiler::target::{Domain, Target};
use cambrian_transpiler::validate::{check_target_compat, validate, Severity};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static OUT_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

fn parse_file(path: &Path) -> Option<cambrian_transpiler::ast::Program> {
    let src = fs::read_to_string(path).ok()?;
    let mut program = cambrian_transpiler::ProgramParser::new()
        .parse(&src)
        .ok()?;
    cambrian_transpiler::ast::normalize_program_types(&mut program);
    Some(program)
}

fn error_codes(program: &cambrian_transpiler::ast::Program, target: Target) -> BTreeSet<String> {
    let mut diags = validate(program);
    diags.extend(check_target_compat(program, target, false));
    diags
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.code.to_string())
        .collect()
}

fn transpiler_bin() -> PathBuf {
    // Compile-time path from Cargo; follows the active target dir
    // (`target/debug`, `target/llvm-cov-target/debug`, …).
    PathBuf::from(env!("CARGO_BIN_EXE_cambrian-transpiler"))
}

fn transpile(path: &Path, target: Target) -> (bool, String) {
    let n = OUT_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
    let out = std::env::temp_dir().join(format!(
        "cambrian-p8-asym-{}-{}-{}-{}",
        target.name(),
        path.file_stem().unwrap_or_default().to_string_lossy(),
        std::process::id(),
        n
    ));
    let _ = fs::remove_dir_all(&out);
    fs::create_dir_all(&out).expect("create temp dir");
    let output = Command::new(transpiler_bin())
        .args([
            path.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
            "--target",
            target.name(),
        ])
        .output()
        .expect("run transpiler");
    let err = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = fs::remove_dir_all(&out);
    (output.status.success(), err)
}

/// Accept = no hard validation errors AND transpile succeeds.
fn accepts(path: &Path, program: &cambrian_transpiler::ast::Program, target: Target) -> (bool, String) {
    let codes = error_codes(program, target);
    if !codes.is_empty() {
        return (false, format!("validate errors: {codes:?}"));
    }
    let (ok, err) = transpile(path, target);
    if ok {
        (true, String::new())
    } else {
        (false, err)
    }
}

fn emitter_cores_of(domain: Domain) -> Vec<Target> {
    Target::emitters()
        .iter()
        .copied()
        .filter(|t| t.domain() == domain)
        .collect()
}

fn multi_core_domains() -> Vec<Domain> {
    [Domain::Container, Domain::Tvm, Domain::Evm]
        .into_iter()
        .filter(|d| emitter_cores_of(*d).len() >= 2)
        .collect()
}

/// Floor on fixtures that accept on at least one core of each multi-core
/// domain. Prevents a silent all-reject regression (PM-011). Bump when the
/// corpus grows; never lower without an explicit allowlist change.
const ACCEPT_BASELINE: &[(Domain, usize)] = &[
    (Domain::Container, 30),
    (Domain::Tvm, 30),
    (Domain::Evm, 30),
];

fn min_accepts_per_domain(domain: Domain) -> usize {
    ACCEPT_BASELINE
        .iter()
        .find(|(d, _)| *d == domain)
        .map(|(_, n)| *n)
        .unwrap_or(0)
}

#[test]
fn domain_fixture_accept_reject_parity() {
    let mut mismatches = Vec::new();
    let mut checked = 0usize;
    let mut accepts_per_domain: std::collections::HashMap<Domain, usize> =
        std::collections::HashMap::new();

    for domain in multi_core_domains() {
        let cores = emitter_cores_of(domain);
        for fx in corpus::domain_fixtures(domain) {
            let Some(program) = parse_file(&fx.path) else {
                mismatches.push(format!("{}: failed to parse", fx.key));
                continue;
            };
            let mut accepted = Vec::new();
            let mut rejected = Vec::new();
            for target in &cores {
                let expected_reject = corpus::is_core_reject(&fx.key, *target);
                let (ok, detail) = accepts(&fx.path, &program, *target);
                match (ok, expected_reject) {
                    (true, false) => accepted.push(target.name()),
                    (false, true) => {
                        // Allowed rejection — still confirm the recorded code
                        // appears when validation is the cause.
                        if let Some(code) = corpus::core_reject_code(&fx.key, *target) {
                            let codes = error_codes(&program, *target);
                            if !codes.is_empty() && !codes.contains(code) {
                                mismatches.push(format!(
                                    "{} on {}: CORE_REJECTS expects {code}, got {codes:?}",
                                    fx.key,
                                    target.name()
                                ));
                            }
                        }
                        rejected.push(format!("{} (allowlisted)", target.name()));
                    }
                    (true, true) => {
                        mismatches.push(format!(
                            "{} on {}: listed in CORE_REJECTS but now accepts — remove the entry",
                            fx.key,
                            target.name()
                        ));
                    }
                    (false, false) => {
                        rejected.push(format!("{}: {}", target.name(), snip(&detail)));
                    }
                }
            }
            if !accepted.is_empty() {
                *accepts_per_domain.entry(domain).or_default() += 1;
            }
            if !accepted.is_empty() && rejected.iter().any(|r| !r.contains("allowlisted")) {
                mismatches.push(format!(
                    "{:?}/{}: accepts {:?} rejects [{}]",
                    domain,
                    fx.key,
                    accepted,
                    rejected.join("; ")
                ));
            }
            // All-reject (no accepts) is fine only if ACCEPT_BASELINE still holds.
            // All-accept is fine.
            // Allowlisted-only rejects with at least one accept is fine.
            checked += 1;
        }
    }

    for domain in multi_core_domains() {
        let got = accepts_per_domain.get(&domain).copied().unwrap_or(0);
        let min = min_accepts_per_domain(domain);
        if got < min {
            mismatches.push(format!(
                "{domain:?}: accept count {got} < ACCEPT_BASELINE / min_accepts_per_domain {min}"
            ));
        }
    }

    assert!(
        mismatches.is_empty(),
        "accept/reject asymmetry across same-domain cores ({checked} fixtures checked):\n\n{}",
        mismatches.join("\n\n")
    );
}

fn snip(s: &str) -> String {
    let line = s
        .lines()
        .find(|l| l.contains("error") || l.contains('['))
        .unwrap_or(s.lines().next().unwrap_or(""));
    line.chars().take(160).collect()
}

fn transpile_project(yaml: &Path) -> (bool, String) {
    // `--project` takes target + output_dir from the YAML (CLI `--target` /
    // `-o` are ignored in project mode). Clean the configured output_dir
    // afterwards so we don't leave build artifacts in the tree.
    let project = match cambrian_transpiler::project::Project::load(yaml) {
        Ok(p) => p,
        Err(e) => return (false, format!("Project::load: {e}")),
    };
    let out = project.output_dir();
    let _ = fs::remove_dir_all(&out);
    let output = Command::new(transpiler_bin())
        .args(["--project", yaml.to_str().unwrap()])
        .output()
        .expect("run transpiler --project");
    let err = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = fs::remove_dir_all(&out);
    (output.status.success(), err)
}

/// Project-level asymmetry gate: dual-core `project.yaml` programs
/// (governor, uniswap-v2, …) must agree on accept/reject across cores,
/// outside [`corpus::CORE_REJECTS`].
#[test]
fn domain_project_accept_reject_parity() {
    let mut mismatches = Vec::new();
    let mut checked = 0usize;

    for (key, domain, yamls) in corpus::dual_core_projects() {
        let mut accepted = Vec::new();
        let mut rejected = Vec::new();
        for (target, yaml) in &yamls {
            let expected_reject = corpus::is_core_reject(&key, *target);
            let project = match cambrian_transpiler::project::Project::load(yaml) {
                Ok(p) => p,
                Err(e) => {
                    mismatches.push(format!("{key} ({target:?}): Project::load failed: {e}"));
                    continue;
                }
            };
            let det = project.config.resolved_deterministic_addresses();
            let mut diags = cambrian_transpiler::validate::validate(&project.merged);
            diags.extend(cambrian_transpiler::validate::check_target_compat(
                &project.merged,
                *target,
                det,
            ));
            let codes: BTreeSet<_> = diags
                .into_iter()
                .filter(|d| d.severity == Severity::Error)
                .map(|d| d.code.to_string())
                .collect();
            let (tp_ok, tp_err) = if codes.is_empty() {
                transpile_project(yaml)
            } else {
                (false, format!("validate errors: {codes:?}"))
            };
            let ok = codes.is_empty() && tp_ok;
            let detail = if codes.is_empty() {
                tp_err
            } else {
                format!("validate errors: {codes:?}")
            };
            match (ok, expected_reject) {
                (true, false) => accepted.push(target.name()),
                (false, true) => rejected.push(format!("{} (allowlisted)", target.name())),
                (true, true) => mismatches.push(format!(
                    "{key} on {}: listed in CORE_REJECTS but now accepts — remove the entry",
                    target.name()
                )),
                (false, false) => {
                    rejected.push(format!("{}: {}", target.name(), snip(&detail)));
                }
            }
        }
        if !accepted.is_empty() && rejected.iter().any(|r| !r.contains("allowlisted")) {
            mismatches.push(format!(
                "{:?}/{key}: accepts {:?} rejects [{}]",
                domain,
                accepted,
                rejected.join("; ")
            ));
        }
        checked += 1;
    }

    assert!(
        !corpus::DUAL_CORE_PROJECTS.is_empty(),
        "DUAL_CORE_PROJECTS must list at least one dual-core example"
    );
    assert!(
        mismatches.is_empty(),
        "project accept/reject asymmetry ({checked} projects checked):\n\n{}",
        mismatches.join("\n\n")
    );
}
