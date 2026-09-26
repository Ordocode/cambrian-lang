// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Lean codegen — orchestrator + `OutputBackend` impl.
//!
//! See `docs/PLAN_LEAN_TARGET.md` for the phased plan. This module is
//! the public face of the Lean cluster (analogous to `codegen::evm`)
//! and currently emits the **P0 scaffolding**: a Lake project with
//! the vendored `Cambrian.Prelude`, a per-entity `Cambrian/Generated/<E>.lean`
//! file containing namespace + empty `State` / `Identity` / projection,
//! and the `lean-toolchain` pin.

pub mod core;
mod entity;
pub mod evm;
pub(crate) mod expr;
mod member;
mod route;

use crate::ast::{Entity, Program, Route};
use crate::project::{Project, ProjectConfig};

use self::core::pure::gen_pure_module;
use self::entity::{gen_all_routes_files, gen_entity};
use self::evm::dispatch::gen_dispatch_module;
use self::evm::extern_::gen_extern_module;
use self::evm::spec::{entity_has_specs, gen_spec_module};
use self::evm::world::gen_world_module;
use super::adapter::OutputBackend;

/// Default Lean toolchain pinned by the transpiler. Users can override
/// via `project.yaml`'s future `lean.lean_toolchain` field (P0 ships
/// only the constant).
pub const DEFAULT_LEAN_TOOLCHAIN: &str = "leanprover/lean4:v4.29.1";

/// Hermetically vendored domain-free Core (`Cambrian/Core.lean`).
const CORE_LEAN: &str = include_str!("../../../assets/lean-prelude/Cambrian/Core.lean");

/// Hermetically vendored EVM-domain module (`Cambrian/Evm.lean`) — MsgCtx,
/// WorldState, and the swappable crypto-intrinsic region.
const EVM_LEAN: &str = include_str!("../../../assets/lean-prelude/Cambrian/Evm.lean");

/// Thin Prelude facade (`import Core` + `import Evm`). Emitters still write
/// `Cambrian/Prelude.lean` so existing `import Cambrian.Prelude` keeps working.
const PRELUDE_LEAN: &str = include_str!("../../../assets/lean-prelude/Cambrian/Prelude.lean");

/// Hermetically vendored `Cambrian.SimpAttrs.lean` — registers the
/// Cambrian `simp` sets in a base module Core imports (a
/// `register_simp_attr` set cannot be applied in its own module).
const SIMP_ATTRS_LEAN: &str = include_str!("../../../assets/lean-prelude/Cambrian/SimpAttrs.lean");

/// Executable EVM-intrinsic reference models (computable, deterministic,
/// non-cryptographic). Spliced into `EVM_LEAN`'s intrinsics region to
/// produce the executable-intrinsics prelude variant.
const INTRINSICS_EXEC_LEAN: &str =
    include_str!("../../../assets/lean-prelude/Cambrian/IntrinsicsExec.lean");

/// Escrow-profile Prelude support (phase B, item B4): the `Cambrian.tryOr`
/// combinator the predictable route bodies reference. Appended to the emitted
/// `Cambrian/Prelude.lean` facade ONLY when `lean.emission_profile: predictable`
/// is active (see [`prelude_with_predictable`]); the default profile emits the
/// facade byte-identically without it.
#[cfg(feature = "predictable-profile")]
const PREDICTABLE_LEAN: &str = include_str!("../../../assets/lean-prelude/Cambrian/Predictable.lean");

/// Sentinel marking the start of the swappable crypto-intrinsic region in
/// `EVM_LEAN` (kept in the output; only the text *between* the markers is
/// replaced).
const INTRINSICS_BEGIN: &str = "-- CAMBRIAN:INTRINSICS:BEGIN\n";
/// Sentinel marking the end of the swappable crypto-intrinsic region.
const INTRINSICS_END: &str = "-- CAMBRIAN:INTRINSICS:END";

/// Same grant as generated Solidity: emitted support code is not GPL.
/// The user chooses the license for the Lake project (see README).
const EMITTED_SUPPORT_SPDX: &str = "-- SPDX-License-Identifier: UNLICENSED\n";

/// Drop a leading Cambrian copyright / SPDX block from a vendored Lean asset.
/// Only leading lines are removed so a splice fragment cannot leak a GPL
/// header into the middle of another module.
fn strip_repo_license_header(src: &str) -> &str {
    let mut rest = src;
    loop {
        let after_ws = rest.trim_start_matches(['\n', '\r']);
        let line_end = after_ws.find('\n').unwrap_or(after_ws.len());
        let line = after_ws[..line_end].trim_end_matches('\r').trim_end();
        if line.starts_with("-- Copyright") || line.starts_with("-- SPDX-License-Identifier:") {
            rest = if line_end < after_ws.len() {
                &after_ws[line_end + 1..]
            } else {
                ""
            };
            continue;
        }
        break;
    }
    rest.trim_start_matches(['\n', '\r'])
}

/// Prepend [`EMITTED_SUPPORT_SPDX`] to an already-stripped Lean body.
fn with_unlicensed_spdx(body: &str) -> String {
    let body = body.trim_start_matches(['\n', '\r']);
    let mut out = String::with_capacity(EMITTED_SUPPORT_SPDX.len() + body.len() + 1);
    out.push_str(EMITTED_SUPPORT_SPDX);
    out.push_str(body);
    if !body.is_empty() && !body.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Emit a standalone Lean support module (prelude, SimpAttrs, plausible
/// harness): strip the in-repo GPL header and tag the copy `UNLICENSED`.
pub(crate) fn emit_support_lean(src: &str) -> String {
    with_unlicensed_spdx(strip_repo_license_header(src))
}

/// Vendored Core module (domain-free). Header rewritten on emit; body is
/// the in-repo asset.
fn core_lean() -> String {
    emit_support_lean(CORE_LEAN)
}

/// Thin Prelude facade (`import SimpAttrs` + Core + Evm). Always emitted
/// as `Cambrian/Prelude.lean`; predictable support may be appended via [`prelude_with_predictable`].
fn prelude_facade() -> String {
    emit_support_lean(PRELUDE_LEAN)
}

/// Vendored Evm module with opaque intrinsics (FV default).
fn evm_opaque() -> String {
    emit_support_lean(EVM_LEAN)
}

/// Evm module with its opaque intrinsic region replaced by the executable
/// reference models (`IntrinsicsExec.lean`). The `BEGIN`/`END` marker lines are
/// preserved as anchors. Panics only on a malformed vendored asset (missing
/// markers), which a transpiler unit test guards against.
fn evm_executable() -> String {
    let evm = strip_repo_license_header(EVM_LEAN);
    let begin_at = evm
        .find(INTRINSICS_BEGIN)
        .expect("Evm.lean must contain the CAMBRIAN:INTRINSICS:BEGIN marker");
    let inner_start = begin_at + INTRINSICS_BEGIN.len();
    let end_at = evm[inner_start..]
        .find(INTRINSICS_END)
        .map(|off| inner_start + off)
        .expect("Evm.lean must contain the CAMBRIAN:INTRINSICS:END marker");
    let splice = strip_repo_license_header(INTRINSICS_EXEC_LEAN);
    let mut body = String::with_capacity(evm.len() + splice.len());
    body.push_str(&evm[..inner_start]);
    body.push_str(splice);
    if !splice.ends_with('\n') {
        body.push('\n');
    }
    body.push_str(&evm[end_at..]);
    with_unlicensed_spdx(&body)
}

/// Append the predictable-profile support section ([`PREDICTABLE_LEAN`], marked
/// `-- CAMBRIAN:PREDICTABLE:BEGIN/END`) to the Prelude facade. Called only when
/// the predictable emission profile is active, so the legacy facade stays
/// byte-identical. Escrow defs land in module `Cambrian.Prelude` after the
/// Core/Evm imports, so `import Cambrian.Prelude` still sees them.
fn prelude_with_predictable(base: String) -> String {
    #[cfg(feature = "predictable-profile")]
    {
        let mut out = base;
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
        let extra = strip_repo_license_header(PREDICTABLE_LEAN);
        out.push_str(extra);
        if !extra.ends_with('\n') {
            out.push('\n');
        }
        out
    }
    #[cfg(not(feature = "predictable-profile"))]
    {
        base
    }
}

/// Emit the three prelude modules (Core / Evm / Prelude facade), honouring
/// `exec` for the Evm intrinsic splice and optionally appending predictable support to
/// the facade. Shared by single-file `extra_files` and project emission.
fn emit_prelude_files(exec: bool, predictable: bool) -> Vec<(String, String)> {
    let evm = if exec { evm_executable() } else { evm_opaque() };
    let prelude = if predictable {
        prelude_with_predictable(prelude_facade())
    } else {
        prelude_facade()
    };
    vec![
        ("Cambrian/Core.lean".to_string(), core_lean()),
        ("Cambrian/Evm.lean".to_string(), evm),
        ("Cambrian/Prelude.lean".to_string(), prelude),
    ]
}
/// True when `project.yaml` sets `lean.numerics: nat`.
pub fn lean_use_nat_numerics(config: &ProjectConfig) -> bool {
    matches!(
        config.resolved_lean().numerics.as_deref(),
        Some("nat" | "Nat" | "NAT")
    )
}

/// True when `project.yaml` sets `lean.numerics: overflow-panic`.
/// Absent / `overflow-wrap` / legacy `bitvec` keep wrapping BitVec arithmetic.
pub fn lean_use_overflow_panic(config: &ProjectConfig) -> bool {
    matches!(
        config.resolved_lean().numerics.as_deref(),
        Some("overflow-panic")
    )
}

/// True when `project.yaml` sets `lean.intrinsics: executable`, selecting the
/// computable EVM-intrinsic reference models for the *main* prelude. Defaults
/// to `false` (opaque models, sound for formal verification).
pub fn lean_use_executable_intrinsics(config: &ProjectConfig) -> bool {
    matches!(
        config.resolved_lean().intrinsics.as_deref(),
        Some("executable" | "Executable" | "exec")
    )
}

/// True when `project.yaml` sets `lean.emission_profile: predictable` — the
/// pinned emission profile for PoT digest prediction (phase B,
/// `CAMBRIAN_R3_SPEC.md` §7). Absent or `"default"` (the only other
/// accepted value) keeps today's emission untouched. Any other string is
/// rejected earlier, at project-config-validation time
/// (`validate::validate_project_config`, code `F2`) — by the time codegen
/// calls this, the value is guaranteed to be `None`, `"default"`, or
/// `"predictable"`.
pub fn lean_use_predictable_profile(config: &ProjectConfig) -> bool {
    #[cfg(feature = "predictable-profile")]
    {
        matches!(
            config.resolved_lean().emission_profile.as_deref(),
            Some("predictable")
        )
    }
    #[cfg(not(feature = "predictable-profile"))]
    {
        let _ = config;
        false
    }
}

/// Predictor mirror of `route_local::transform_body_checked`. `overflow_panic`
/// and `nat_numerics` must match the **project** under analysis — do not rely on
/// TLS `use_nat_numerics()` (stale after a prior `gen_project` with different
/// `lean.numerics`; see T-ARCH-020 / RV-2).
pub fn member_transform_body_checked(
    body: &crate::ast::Expr,
    pures: &[crate::ast::PureFn],
    overflow_panic: bool,
    nat_numerics: bool,
) -> bool {
    expr::expr_forces_fail_surface_with_nat(body, pures, nat_numerics)
        || (overflow_panic && !nat_numerics && expr::expr_has_checked_binop(body))
}

/// Predictor / external mirror of [`expr::expr_forces_fail_surface`] (TLS numerics).
pub fn expr_forces_fail_surface(body: &crate::ast::Expr, pures: &[crate::ast::PureFn]) -> bool {
    expr::expr_forces_fail_surface(body, pures)
}

/// Predictor / external mirror with explicit `lean.numerics: nat` flag.
pub fn expr_forces_fail_surface_with_nat(
    body: &crate::ast::Expr,
    pures: &[crate::ast::PureFn],
    nat_numerics: bool,
) -> bool {
    expr::expr_forces_fail_surface_with_nat(body, pures, nat_numerics)
}

/// Predictor / external mirror of [`expr::pure_fn_forces_fail`].
pub fn pure_fn_forces_fail(f: &crate::ast::PureFn, pures: &[crate::ast::PureFn]) -> bool {
    expr::pure_fn_forces_fail(f, pures)
}

/// True when any member transform on `route` is a div-by-zero / narrowing fail
/// surface (predictor mirror of `route.rs::route_has_div0_or_narrow`'s transform
/// scan — body actions are covered separately by `route_can_fail_evm_lean`).
pub fn route_transforms_force_fail_surface(
    program: &Program,
    entity: &Entity,
    route: &Route,
) -> bool {
    for m in &entity.members {
        for tr in &m.transforms {
            if tr.route_name == route.name
                && expr::expr_forces_fail_surface(&tr.body, &program.pure_fns)
            {
                return true;
            }
        }
    }
    false
}

thread_local! {
    /// Per-run predictable emission flag. Seeded at the start of each
    /// `OutputBackend` entry point (and mirrored into `LeanProfile` /
    /// `LeanTypeCtx.use_predictable_profile`). Used by sites that lack a
    /// profile/`LeanExprCtx` in hand — e.g. `lower_let_binding`, iterator
    /// closure binders, and the predictable term rewriter.
    static USE_PREDICTABLE_PROFILE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Per-run `lean.numerics: overflow-panic` flag (mirrors
    /// `LeanProfile.overflow_panic`). Used by `route_fail_mode` when
    /// promoting routes whose transforms use checked `+`/`-`/`*`.
    static USE_OVERFLOW_PANIC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Per-run `lean.numerics: nat`. Div0/narrow fail-mode is BitVec-only
    /// (`nat` `/ 0` and saturating `-` are the INTENDED proof model, PW3-O-002).
    static USE_NAT_NUMERICS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Set the per-run predictable emission flag (called at the start of each backend
/// entry point).
pub(crate) fn set_use_predictable_profile(on: bool) {
    USE_PREDICTABLE_PROFILE.with(|c| c.set(on));
}

/// Whether the predictable emission profile is active for the current run.
pub(crate) fn use_predictable_profile() -> bool {
    USE_PREDICTABLE_PROFILE.with(|c| c.get())
}

pub(crate) fn set_use_overflow_panic(on: bool) {
    USE_OVERFLOW_PANIC.with(|c| c.set(on));
}

pub(crate) fn use_overflow_panic() -> bool {
    USE_OVERFLOW_PANIC.with(|c| c.get())
}

pub(crate) fn set_use_nat_numerics(on: bool) {
    USE_NAT_NUMERICS.with(|c| c.set(on));
}

pub(crate) fn use_nat_numerics() -> bool {
    USE_NAT_NUMERICS.with(|c| c.get())
}

/// Resolve `lean.proof_helpers` from project config (defaults to `true`).
pub fn lean_emit_proof_helpers(config: &ProjectConfig) -> bool {
    config.resolved_lean().proof_helpers.unwrap_or(true)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeanProfile {
    pub proof_helpers: bool,
    pub nat_numerics: bool,
    /// `lean.numerics: overflow-panic` — checked BitVec `+`/`-`/`*`.
    pub overflow_panic: bool,
    pub predictable: bool,
    /// Mirror of `project.yaml` `deterministic_addresses` (default `false`).
    pub deterministic_addresses: bool,
}

impl LeanProfile {
    pub const DEFAULT: Self = Self {
        proof_helpers: true,
        nat_numerics: false,
        overflow_panic: false,
        predictable: false,
        deterministic_addresses: false,
    };

    pub fn from_config(config: &ProjectConfig) -> Self {
        Self {
            proof_helpers: lean_emit_proof_helpers(config),
            nat_numerics: lean_use_nat_numerics(config),
            overflow_panic: lean_use_overflow_panic(config),
            predictable: lean_use_predictable_profile(config),
            deterministic_addresses: config.deterministic_addresses.unwrap_or(false),
        }
    }
}

/// Lean 4 + Lake `OutputBackend`. P0 emits a self-contained Lake
/// project per Cambrian project (or per single `.cam` file in
/// single-file mode).
pub struct LeanBackend {
    /// Toolchain pin written to `lean-toolchain`.
    pub lean_toolchain: String,
    /// Whether to copy the vendored `Cambrian/Prelude.lean` into the
    /// generated project. Defaults to `true`; tests may flip this off
    /// to keep golden output focused.
    pub include_prelude: bool,
}

impl Default for LeanBackend {
    fn default() -> Self {
        LeanBackend {
            lean_toolchain: DEFAULT_LEAN_TOOLCHAIN.to_string(),
            include_prelude: true,
        }
    }
}

impl LeanBackend {
    /// Generate the contents of `lakefile.toml`. P0 keeps this minimal
    /// (single library `Cambrian`, no Mathlib) — P1+ revisits it when
    /// the Prelude starts depending on Mathlib.
    fn gen_lakefile_toml(&self) -> String {
        // Lake supports both `lakefile.lean` and `lakefile.toml`. We use
        // `.toml` because it is purely declarative and doesn't need a
        // Lean parser to round-trip.
        let mut s = String::new();
        s.push_str("# Auto-generated by cambrian-transpiler — Lean target (P0).\n");
        s.push_str("# Bumping any field here is a transpiler PR.\n\n");
        s.push_str("name = \"cambrian-generated\"\n");
        s.push_str("defaultTargets = [\"Cambrian\"]\n\n");
        s.push_str("[[lean_lib]]\n");
        s.push_str("name = \"Cambrian\"\n");
        s
    }

    fn gen_lean_toolchain(&self) -> String {
        // `lean-toolchain` is a single-line file; trailing newline is
        // standard.
        let mut s = String::with_capacity(self.lean_toolchain.len() + 1);
        s.push_str(&self.lean_toolchain);
        s.push('\n');
        s
    }

    /// Generate the `Cambrian.lean` library-root file that re-imports
    /// the prelude and every generated entity. Lake resolves a
    /// `[[lean_lib]] name = "Cambrian"` declaration by looking for
    /// either `Cambrian.lean` or per-module roots; we use the former
    /// so adding new entities only requires regenerating this file.
    fn gen_lean_root(&self, program: &Program, profile: LeanProfile) -> String {
        let mut s = String::new();
        s.push_str("-- Auto-generated by cambrian-transpiler. Do not edit.\n");
        s.push_str("-- Library root: re-imports the vendored prelude and every\n");
        s.push_str("-- generated entity so `lake build` builds the full project.\n");
        s.push_str("import Cambrian.Prelude\n");
        if self::core::pure::program_needs_pure_import(program) {
            s.push_str("import Cambrian.Generated.Pure\n");
        }
        for entity in &program.entities {
            s.push_str(&format!("import Cambrian.Generated.{}\n", entity.name));
        }
        s.push_str("import Cambrian.Generated.World\n");
        if !program.extern_entities.is_empty() {
            s.push_str("import Cambrian.Generated.Extern\n");
        }
        if !gen_dispatch_module(program, profile).is_empty() {
            s.push_str("import Cambrian.Generated.Dispatch\n");
        }
        for entity in &program.entities {
            if !entity.routes.is_empty() {
                s.push_str(&format!(
                    "import Cambrian.Generated.{}Routes\n",
                    entity.name
                ));
            }
        }
        // Under the predictable profile each `<E>Spec` imports a hermetic
        // `<E>Statements` sidecar (the lifted theorem types); register it in
        // the library root ahead of its Spec, exactly as every other generated
        // module is registered. Legacy stays byte-identical (no sidecar).
        let predictable = profile.predictable;
        for entity in &program.entities {
            if entity_has_specs(program, entity) {
                if predictable {
                    s.push_str(&format!(
                        "import Cambrian.Generated.{}Statements\n",
                        entity.name
                    ));
                }
                s.push_str(&format!("import Cambrian.Generated.{}Spec\n", entity.name));
            }
        }
        s
    }
}

impl OutputBackend for LeanBackend {
    fn gen_program(&self, program: &Program) -> String {
        // Single-file mode has no `project.yaml`, so proof helpers default
        // on, numerics stay BitVec, and the emission profile stays legacy.
        let profile = LeanProfile::DEFAULT;
        set_use_predictable_profile(false);
        set_use_overflow_panic(false);
        set_use_nat_numerics(false);
        let program = &*core::pure::resolve_library_refs(program);
        // Single-file mode: emit just the (first) entity's Lean file.
        // The Lake scaffolding is produced via `extra_files` so that
        // single-file callers still get a buildable project.
        program
            .entities
            .first()
            .map(|e| gen_entity(program, e, profile))
            .unwrap_or_default()
    }

    fn file_extension(&self) -> &str {
        "lean"
    }

    fn file_name_for_entity(&self, entity_name: &str) -> String {
        format!("Cambrian/Generated/{}.lean", entity_name)
    }

    fn extra_files(&self, program: &Program, _entity_name: &str) -> Vec<(String, String)> {
        let resolved = core::pure::resolve_library_refs(program);
        let program = &*resolved;
        // Single-file mode: no project config, so proof helpers default on,
        // numerics stay BitVec, and the emission profile stays legacy.
        let profile = LeanProfile::DEFAULT;
        set_use_predictable_profile(false);
        set_use_overflow_panic(false);
        set_use_nat_numerics(false);
        let mut files = vec![
            ("lakefile.toml".to_string(), self.gen_lakefile_toml()),
            ("lean-toolchain".to_string(), self.gen_lean_toolchain()),
            (
                "Cambrian.lean".to_string(),
                self.gen_lean_root(program, profile),
            ),
        ];
        if self.include_prelude {
            files.push((
                "Cambrian/SimpAttrs.lean".to_string(),
                emit_support_lean(SIMP_ATTRS_LEAN),
            ));
            // Single-file mode has no project config: keep the opaque
            // (FV-sound) intrinsics and emit no plausible sidecar.
            files.extend(emit_prelude_files(
                /* exec */ false, /* predictable */ false,
            ));
        }
        if let Some(pure) = gen_pure_module(program, profile) {
            files.push(("Cambrian/Generated/Pure.lean".to_string(), pure));
        }
        files.push((
            "Cambrian/Generated/World.lean".to_string(),
            gen_world_module(program, profile),
        ));
        let extern_mod = gen_extern_module(program, profile);
        if !extern_mod.is_empty() {
            files.push(("Cambrian/Generated/Extern.lean".to_string(), extern_mod));
        }
        let dispatch_mod = gen_dispatch_module(program, profile);
        if !dispatch_mod.is_empty() {
            files.push(("Cambrian/Generated/Dispatch.lean".to_string(), dispatch_mod));
        }
        for entity in &program.entities {
            files.push((
                format!("Cambrian/Generated/{}.lean", entity.name),
                gen_entity(program, entity, profile),
            ));
        }
        files.extend(gen_all_routes_files(program, profile));
        for entity in &program.entities {
            if let Some(spec) = gen_spec_module(program, entity, profile) {
                // Statements sidecar (predictable profile only; single-file mode is
                // always legacy, so this stays `None` here) precedes its Spec.
                if let Some(stmts) = spec.statements {
                    files.push((
                        format!("Cambrian/Generated/{}Statements.lean", entity.name),
                        stmts,
                    ));
                }
                files.push((
                    format!("Cambrian/Generated/{}Spec.lean", entity.name),
                    spec.spec,
                ));
            }
        }
        files
    }

    fn target_description(&self) -> &str {
        "Lean 4"
    }

    fn gen_project(&self, project: &Project) -> Vec<(String, String)> {
        let profile = LeanProfile::from_config(&project.config);
        let merged = core::pure::resolve_library_refs(&project.merged);
        set_use_predictable_profile(profile.predictable);
        set_use_overflow_panic(profile.overflow_panic);
        set_use_nat_numerics(profile.nat_numerics);
        #[cfg(feature = "plausible")]
        let emit_overlay = evm::plausible::lean_emit_plausible_overlay(&project.config);
        let mut all_files = vec![
            ("lakefile.toml".to_string(), self.gen_lakefile_toml()),
            ("lean-toolchain".to_string(), self.gen_lean_toolchain()),
            (
                "Cambrian.lean".to_string(),
                self.gen_lean_root(&merged, profile),
            ),
        ];
        if self.include_prelude {
            all_files.push((
                "Cambrian/SimpAttrs.lean".to_string(),
                emit_support_lean(SIMP_ATTRS_LEAN),
            ));
            // The *main* Evm module honours `lean.intrinsics` (default opaque,
            // so `lake build` in the generated tree stays FV-sound).
            let exec = lean_use_executable_intrinsics(&project.config);
            all_files.extend(emit_prelude_files(exec, profile.predictable));
            // Backward-compat executable-intrinsics sidecar for the legacy
            // sync-based overlays. When the self-contained overlay is emitted
            // it carries its own copy, so the bare sidecar is skipped to avoid
            // a stray module in the overlay directory.
            #[cfg(feature = "plausible")]
            if !emit_overlay {
                all_files.push(("plausible/Evm.lean".to_string(), evm_executable()));
            }
        }
        if let Some(pure) = gen_pure_module(&merged, profile) {
            all_files.push(("Cambrian/Generated/Pure.lean".to_string(), pure));
        }
        all_files.push((
            "Cambrian/Generated/World.lean".to_string(),
            gen_world_module(&merged, profile),
        ));
        let extern_mod = gen_extern_module(&merged, profile);
        if !extern_mod.is_empty() {
            all_files.push(("Cambrian/Generated/Extern.lean".to_string(), extern_mod));
        }
        let dispatch_mod = gen_dispatch_module(&merged, profile);
        if !dispatch_mod.is_empty() {
            all_files.push(("Cambrian/Generated/Dispatch.lean".to_string(), dispatch_mod));
        }
        for entity in &merged.entities {
            all_files.push((
                format!("Cambrian/Generated/{}.lean", entity.name),
                gen_entity(&merged, entity, profile),
            ));
        }
        all_files.extend(gen_all_routes_files(&merged, profile));
        for entity in &merged.entities {
            if let Some(spec) = gen_spec_module(&merged, entity, profile) {
                // Statements sidecar (predictable profile) must precede the Spec
                // file that imports it in module order.
                if let Some(stmts) = spec.statements {
                    all_files.push((
                        format!("Cambrian/Generated/{}Statements.lean", entity.name),
                        stmts,
                    ));
                }
                all_files.push((
                    format!("Cambrian/Generated/{}Spec.lean", entity.name),
                    spec.spec,
                ));
            }
        }
        // Per-property `fuzz` sampling-range sidecar (never leaked into the
        // theorems themselves).
        #[cfg(feature = "plausible")]
        if let Some(bounds) = evm::bounds::gen_plausible_bounds_json(&merged, profile) {
            all_files.push(("plausible-bounds.json".to_string(), bounds));
        }
        // Self-contained runnable Plausible overlay (single source of truth for
        // the examples, SMAFD's pre-hook, and CI). Built from the FV tree above
        // with the executable-intrinsic prelude swapped in.
        #[cfg(feature = "plausible")]
        if emit_overlay {
            let overlay =
                evm::plausible::gen_plausible_overlay(project, &all_files, self.gen_lean_toolchain());
            all_files.extend(overlay);
        }
        all_files
    }
}

#[cfg(test)]
mod emitted_support_license_tests {
    use super::*;

    #[test]
    fn strip_drops_leading_copyright_and_spdx() {
        let src = "-- Copyright (C) 2025-2026 The Cambrian Authors\n-- SPDX-License-Identifier: GPL-3.0-only\n\nimport Foo\n";
        assert_eq!(strip_repo_license_header(src), "import Foo\n");
    }

    #[test]
    fn emit_support_lean_is_unlicensed_not_gpl() {
        let out = emit_support_lean(CORE_LEAN);
        assert!(
            out.starts_with(EMITTED_SUPPORT_SPDX),
            "emitted Core must start with UNLICENSED SPDX:\n{out}"
        );
        assert!(
            !out.contains("GPL-3.0-only"),
            "emitted Core must not carry the repo GPL identifier:\n{}",
            &out[..out.len().min(400)]
        );
        assert!(
            !out.contains("Copyright (C)"),
            "emitted Core must not carry the repo copyright line:\n{}",
            &out[..out.len().min(400)]
        );
        assert!(out.contains("namespace Cambrian"));
    }

    #[test]
    fn executable_evm_splice_does_not_reintroduce_gpl() {
        let out = evm_executable();
        assert!(out.starts_with(EMITTED_SUPPORT_SPDX));
        assert!(!out.contains("GPL-3.0-only"), "{out}");
        assert!(out.contains(INTRINSICS_BEGIN));
        assert!(out.contains(INTRINSICS_END));
    }
}
