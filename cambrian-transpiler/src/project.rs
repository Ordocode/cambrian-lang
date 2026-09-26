// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::ast::{self, Program};
use crate::diagnostic::{diagnostic_from_parse_error, format_human};
use crate::parse_recover::recover_parse_diagnostics;
use crate::validate::Diagnostic;

// ===========================================================================
// YAML config types
// ===========================================================================

/// Field names of a `#[derive(Deserialize)]` struct, captured from the
/// `deserialize_struct` call the derive makes.
fn struct_fields<T: serde::de::DeserializeOwned>() -> &'static [&'static str] {
    use serde::de::{self, Visitor};
    struct FieldsDe<'a>(&'a mut &'static [&'static str]);
    impl<'de> de::Deserializer<'de> for FieldsDe<'_> {
        type Error = de::value::Error;
        fn deserialize_any<V: Visitor<'de>>(self, _: V) -> Result<V::Value, Self::Error> {
            Err(de::Error::custom("field introspection"))
        }
        fn deserialize_struct<V: Visitor<'de>>(
            self,
            _: &'static str,
            fields: &'static [&'static str],
            _: V,
        ) -> Result<V::Value, Self::Error> {
            *self.0 = fields;
            Err(de::Error::custom("field introspection"))
        }
        serde::forward_to_deserialize_any! {
            bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes
            byte_buf option unit unit_struct newtype_struct seq tuple tuple_struct map
            enum identifier ignored_any
        }
    }
    let mut fields: &'static [&'static str] = &[];
    let _ = T::deserialize(FieldsDe(&mut fields));
    fields
}

/// F7: unknown keys at the top level and inside every known block, plus
/// keys that are parsed but have no effect.
pub fn config_key_warnings(yaml: &str) -> Vec<String> {
    let Ok(serde_yml::Value::Mapping(top)) = serde_yml::from_str::<serde_yml::Value>(yaml) else {
        return vec![];
    };
    fn unknown(
        map: &serde_yml::Mapping,
        known: &[&str],
        path: &str,
        out: &mut Vec<String>,
    ) {
        for k in map.keys() {
            if let Some(k) = k.as_str() {
                if !known.contains(&k) {
                    out.push(format!("unknown key `{path}{k}` (ignored)"));
                }
            }
        }
    }
    let block = |name: &str| match top.get(name) {
        Some(serde_yml::Value::Mapping(m)) => Some(m),
        _ => None,
    };
    let mut out = Vec::new();
    unknown(&top, struct_fields::<ProjectConfig>(), "", &mut out);
    let blocks: [(&str, &[&str]); 7] = [
        ("foundry", struct_fields::<FoundryConfig>()),
        ("revm_tests", struct_fields::<RevmTestConfig>()),
        ("ackinacki", struct_fields::<AckiNackiTestConfig>()),
        ("fuzz", struct_fields::<FuzzConfig>()),
        ("invariant", struct_fields::<InvariantConfig>()),
        ("lean", struct_fields::<LeanConfig>()),
        ("evm", struct_fields::<EvmConfig>()),
    ];
    for (name, known) in blocks {
        if let Some(m) = block(name) {
            unknown(m, known, &format!("{name}."), &mut out);
            if let Some(serde_yml::Value::Mapping(cov)) = m.get("coverage") {
                unknown(
                    cov,
                    struct_fields::<CoverageFuzzConfig>(),
                    &format!("{name}.coverage."),
                    &mut out,
                );
            }
        }
    }
    if block("fuzz").is_some_and(|m| m.contains_key("shrink")) {
        out.push("`fuzz.shrink` is accepted but has no effect on any backend".to_string());
    }
    out
}

fn default_output_dir() -> String {
    "build/".to_string()
}

#[derive(Debug, Deserialize, Default)]
pub struct FoundryConfig {
    pub solc_version: Option<String>,
    pub evm_version: Option<String>,
    pub optimizer: Option<bool>,
    pub optimizer_runs: Option<u32>,
    pub via_ir: Option<bool>,
    pub fuzz_runs: Option<u32>,
    /// Optional override map for per-profile fuzz/invariant tuning. The
    /// emitter always produces `[profile.cambrian]` (cheap CI cycle) and
    /// `[profile.cambrian_night]` (overnight stress) profiles; users
    /// can override the defaults here without writing the `foundry.toml`
    /// by hand.
    #[serde(default)]
    pub profiles: Option<std::collections::HashMap<String, ProfileTuning>>,
    /// Phase Library-4: Solidity remappings emitted into `foundry.toml`'s
    /// `remappings = [...]` array. Allows projects to point
    /// `@solidity_import("@openzeppelin/contracts/...")` at a checked-out
    /// OpenZeppelin clone (`lib/openzeppelin-contracts/contracts/`). Each
    /// entry is a literal `"prefix=path"` string passed through verbatim.
    #[serde(default)]
    pub remappings: Option<Vec<String>>,
}

/// Per-profile fuzz/invariant tuning knobs. All fields optional — when
/// absent, the emitter picks the built-in default for the profile.
#[derive(Debug, Deserialize, Default, Clone)]
pub struct ProfileTuning {
    pub fuzz_runs: Option<u32>,
    pub invariant_runs: Option<u32>,
    pub invariant_depth: Option<u32>,
    pub fail_on_revert: Option<bool>,
}

/// Configuration for the revm-based Rust test backend.
/// When enabled, generates a `revm-tests/` Rust crate alongside the
/// existing Foundry test pipeline. Both backends can be used together.
#[derive(Debug, Deserialize, Default)]
pub struct RevmTestConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Optional: also generate proptest fuzz harnesses (future).
    #[serde(default)]
    pub proptest: bool,
    /// Path to the `solc` binary used by the generated `build.rs`.
    /// Defaults to `solc` (looked up on PATH).
    #[serde(default)]
    pub solc_path: Option<String>,
    /// Solc version string passed to the generated crate (informational).
    #[serde(default)]
    pub solc_version: Option<String>,
    /// EVM version target for solc (defaults to `prague`).
    #[serde(default)]
    pub evm_version: Option<String>,
    /// When set and `enabled`, emit a sibling `revm-tests/fuzz/` cargo-fuzz
    /// sub-crate that reuses the same Action enum / dispatcher as the
    /// proptest harness but is driven by libFuzzer / AFL coverage feedback.
    #[serde(default)]
    pub coverage: Option<CoverageFuzzConfig>,
}

/// Configuration for the Acki Nacki host-Rust test backend. The crate itself
/// is always emitted (it is the WASM component crate); this block only
/// controls optional sidecar pipelines like coverage-guided fuzzing.
#[derive(Debug, Deserialize, Default, Clone)]
pub struct AckiNackiTestConfig {
    /// When set and `enabled`, emit a sibling `<entity>-entity-wasm/fuzz/`
    /// cargo-fuzz sub-crate that drives `execute_impl` via libFuzzer / AFL.
    #[serde(default)]
    pub coverage: Option<CoverageFuzzConfig>,
}

/// Coverage-guided input fuzzing knobs, shared by all Rust-host backends.
#[derive(Debug, Deserialize, Clone)]
pub struct CoverageFuzzConfig {
    /// When false (default), no fuzz sub-crate is emitted even if the parent
    /// `coverage` block is present. Lets users template config without
    /// activating it.
    #[serde(default)]
    pub enabled: bool,
    /// Coverage-guided engine: "libfuzzer" (default) or "afl".
    #[serde(default = "default_coverage_engine")]
    pub engine: String,
    /// Suggested time budget per fuzz run (seconds). Surfaces in the
    /// generated README and as a default `-max_total_time` flag. Default 60.
    #[serde(default = "default_coverage_time_budget")]
    pub time_budget_secs: u64,
    /// Optional override for the corpus directory (defaults to
    /// `fuzz/corpus/<target>` per cargo-fuzz convention).
    #[serde(default)]
    pub corpus_dir: Option<String>,
    /// libFuzzer `-seed` value; 0 = nondeterministic. Default 0.
    #[serde(default)]
    pub seed: u64,
    /// Number of parallel fuzzer instances (passed via `cargo fuzz run -j`).
    #[serde(default)]
    pub jobs: Option<u32>,
}

fn default_coverage_engine() -> String {
    "libfuzzer".to_string()
}
fn default_coverage_time_budget() -> u64 {
    60
}

impl Default for CoverageFuzzConfig {
    fn default() -> Self {
        CoverageFuzzConfig {
            enabled: false,
            engine: default_coverage_engine(),
            time_budget_secs: default_coverage_time_budget(),
            corpus_dir: None,
            seed: 0,
            jobs: None,
        }
    }
}

/// Configuration for fuzz/property tests, applies to all backends.
/// Per-target overrides (e.g. `foundry.fuzz_runs`) take precedence when set.
#[derive(Debug, Clone, Deserialize)]
pub struct FuzzConfig {
    /// Number of fuzz iterations per fuzz test (default 256).
    #[serde(default = "default_fuzz_runs")]
    pub runs: u32,
    /// RNG seed: 0 = nondeterministic, non-zero pins the RNG (default 0).
    #[serde(default)]
    pub seed: u64,
    /// Enable shrinking on failure (default true).
    #[serde(default = "default_true")]
    pub shrink: bool,
    /// Maximum number of `assume`/`bound` rejections per case (default 1024).
    #[serde(default = "default_max_local_rejects")]
    pub max_local_rejects: u32,
}

fn default_fuzz_runs() -> u32 {
    256
}
fn default_true() -> bool {
    true
}
fn default_max_local_rejects() -> u32 {
    1024
}

impl Default for FuzzConfig {
    fn default() -> Self {
        FuzzConfig {
            runs: default_fuzz_runs(),
            seed: 0,
            shrink: true,
            max_local_rejects: default_max_local_rejects(),
        }
    }
}

/// Configuration for stateful invariant tests, applies to all backends.
#[derive(Debug, Clone, Deserialize)]
pub struct InvariantConfig {
    /// Number of invariant test traces (default 256).
    #[serde(default = "default_invariant_runs")]
    pub runs: u32,
    /// Maximum number of calls per trace (default 50).
    #[serde(default = "default_invariant_depth")]
    pub depth: u32,
    /// When true, any revert encountered during a trace fails the test.
    /// When false (default), reverting calls are silently skipped.
    #[serde(default)]
    pub fail_on_revert: bool,
    /// RNG seed: 0 = nondeterministic, non-zero pins the RNG (default 0).
    #[serde(default)]
    pub seed: u64,
    /// Maximum number of `assume`/`bound` rejections per trace step (default 1024).
    #[serde(default = "default_max_local_rejects")]
    pub max_local_rejects: u32,
}

fn default_invariant_runs() -> u32 {
    256
}
fn default_invariant_depth() -> u32 {
    50
}

impl Default for InvariantConfig {
    fn default() -> Self {
        InvariantConfig {
            runs: default_invariant_runs(),
            depth: default_invariant_depth(),
            fail_on_revert: false,
            seed: 0,
            max_local_rejects: default_max_local_rejects(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ProjectConfig {
    pub name: Option<String>,
    pub target: String,
    #[serde(default = "default_output_dir")]
    pub output_dir: String,
    pub sources: Vec<String>,
    /// Search roots for library-tier files (`imports:` and bare in-language
    /// `import "…"`). Each entry is env-expanded (`${VAR}`), then treated as
    /// absolute or yaml-relative. Absent / empty is a no-op.
    #[serde(default)]
    pub library_paths: Vec<String>,
    /// Library-tier `.cam` files loaded as [`SourceKind::Imported`] (F4).
    /// Resolved against `base_dir` first, then each `library_paths` root.
    #[serde(default)]
    pub imports: Vec<String>,
    #[serde(default)]
    pub deterministic_addresses: Option<bool>,
    #[serde(default)]
    pub foundry: Option<FoundryConfig>,
    #[serde(default)]
    pub revm_tests: Option<RevmTestConfig>,
    #[serde(default)]
    pub ackinacki: Option<AckiNackiTestConfig>,
    #[serde(default)]
    pub fuzz: Option<FuzzConfig>,
    #[serde(default)]
    pub invariant: Option<InvariantConfig>,
    #[serde(default)]
    pub lean: Option<LeanConfig>,
    #[serde(default)]
    pub evm: Option<EvmConfig>,
    /// F7: keys present in the YAML that no config field reads.
    #[serde(skip)]
    pub key_warnings: Vec<String>,
}

/// EVM-target knobs (`project.yaml` → `evm:` block).
///
/// Example:
/// ```yaml
/// evm:
///   allow_constructor_payable: false
/// ```
///
/// Documented in [`docs/LANGUAGE.md`](../../docs/LANGUAGE.md) and
/// [`cambrian-transpiler/README.md`](../README.md).
#[derive(Debug, Deserialize, Default, Clone)]
pub struct EvmConfig {
    /// When `false`, entity constructors are emitted without `payable` and
    /// deterministic factory CREATE2 deploys use `value: 0`. Default (absent)
    /// is `true` — preserves legacy deploy-with-value behaviour. Designer/agent
    /// policy only; the transpiler does not infer this from program shape.
    #[serde(default)]
    pub allow_constructor_payable: Option<bool>,
}

impl EvmConfig {
    pub fn allow_constructor_payable(&self) -> bool {
        self.allow_constructor_payable.unwrap_or(true)
    }
}

/// Lean-target knobs (`project.yaml` → `lean:` block).
#[derive(Debug, Deserialize, Default, Clone)]
pub struct LeanConfig {
    /// Numeric lowering mode:
    /// - absent / `"overflow-wrap"` — `BitVec n`, wrapping `+`/`-`/`*` (default)
    /// - `"overflow-panic"` — `BitVec n`, checked `+`/`-`/`*` → `RouteResult` fail
    /// - `"nat"` — Lean `Nat`, overflow ignored (proof-friendly)
    ///
    /// Any other string is rejected at project-config validation (`F2`).
    #[serde(default)]
    pub numerics: Option<String>,
    /// Toolchain written to the emitted `lean-toolchain` (and to the
    /// Plausible overlay's copy). Defaults to
    /// [`crate::codegen::lean::DEFAULT_LEAN_TOOLCHAIN`].
    ///
    /// Worth overriding for exactly one reason, and it is a real one: Lake
    /// resolves the toolchain from the ROOT package, so an overlay that
    /// `require`s `plausible-pipeline` drags Mathlib and Batteries built for
    /// whatever that package pins into a build running under whatever the
    /// transpiler pins. When the two drift the failure is a wall of errors
    /// inside Batteries, which reads as a broken dependency rather than as
    /// the version mismatch it is. Pin this to the dependency's toolchain and
    /// the overlay builds.
    #[serde(default)]
    pub lean_toolchain: Option<String>,
    /// Proof-assistance codegen toggle. When `false`, the Lean backend
    /// suppresses *everything generated to help proving* — the reflection
    /// `<route>.Pre` / `<route>_isOk_iff` lemmas, the `cambrian_*_simp`
    /// attribute tags, the per-invariant `invByCases` scaffolding, and the
    /// auto-discharge proof ladders on `test` / `property` / `invariant`
    /// theorems (which then ship statement-only `:= by sorry`). Only the
    /// spec statements themselves and the structural defs needed to state
    /// them remain. Defaults to `true` (helpers emitted). Use this as a
    /// kill switch if a generated helper ever breaks `lake build`.
    #[serde(default)]
    pub proof_helpers: Option<bool>,
    /// EVM crypto-intrinsic lowering for the *main* `Cambrian/Evm.lean`:
    /// `"opaque"` (default — non-executable models + property axioms, sound for
    /// formal verification) or `"executable"` (computable, deterministic,
    /// non-cryptographic reference models so property tests can evaluate
    /// hashes).
    #[serde(default)]
    pub intrinsics: Option<String>,
    /// When `true`, additionally emit a self-contained, runnable property-test
    /// overlay under `<output_dir>/plausible/`. Defaults to `false`; requires a
    /// transpiler built with the `plausible` feature (otherwise ignored).
    #[serde(default)]
    pub plausible: Option<bool>,
    /// Lake `require` path written into the emitted overlay's `lakefile.toml`.
    /// Only used when `plausible` is `true`.
    #[serde(default)]
    pub plausible_pipeline_path: Option<String>,
    /// Umbrella emission-profile knob: `"default"` (when absent) or
    /// `"predictable"`. `default` is today's emission — untouched, byte-identical
    /// output. `predictable` is the pinned emission profile for PoT digest
    /// prediction: a single flag that switches on every predictable-flavored
    /// emission form together (rather than a per-form flag combinatorial
    /// explosion the predictor would each need to validate). Later slices gate
    /// their own forms behind the same flag.
    /// Any value other than `"default"` / `"predictable"` is a hard error at
    /// project-config-validation time (see `validate::validate_project_config`,
    /// code `F2`) — never silently falls back to a default.
    #[serde(default)]
    pub emission_profile: Option<String>,
}

impl ProjectConfig {
    /// Parse `self.target` into a typed [`crate::target::Target`].
    /// Unknown names are an error (no silent native fallback).
    pub fn parsed_target(&self) -> Result<crate::target::Target, String> {
        crate::target::Target::from_name(&self.target).ok_or_else(|| {
            format!(
                "unknown target '{}', expected {}",
                self.target,
                crate::target::Target::expected_names()
            )
        })
    }

    /// Resolved Lean configuration.
    pub fn resolved_lean(&self) -> LeanConfig {
        self.lean.clone().unwrap_or_default()
    }

    /// Resolved fuzz configuration: returns the user-provided block when set,
    /// otherwise the all-defaults `FuzzConfig`.
    pub fn resolved_fuzz(&self) -> FuzzConfig {
        self.fuzz.clone().unwrap_or_default()
    }

    /// Resolved invariant configuration: returns the user-provided block when set,
    /// otherwise the all-defaults `InvariantConfig`.
    pub fn resolved_invariant(&self) -> InvariantConfig {
        self.invariant.clone().unwrap_or_default()
    }

    /// Resolved EVM configuration.
    pub fn resolved_evm(&self) -> EvmConfig {
        self.evm.clone().unwrap_or_default()
    }

    /// Resolved `deterministic_addresses` for the project's target.
    ///
    /// EVM defaults to `true` (mandatory CREATE2 factory deploy, U4-6).
    /// Lean and other targets default to `false`.
    pub fn resolved_deterministic_addresses(&self) -> bool {
        match self.parsed_target() {
            Ok(crate::target::Target::Evm) => self.deterministic_addresses.unwrap_or(true),
            _ => self.deterministic_addresses.unwrap_or(false),
        }
    }
}

// ===========================================================================
// Project — loaded and merged
// ===========================================================================

/// Source provenance — distinguishes project entry-point sources from
/// library-tier files. Imported files are restricted (F4): they may not
/// declare `entity` / `test` / `fuzz` / `invariant`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// Listed in `project.yaml`'s `sources:` (or the CLI's entry .cam).
    Entry,
    /// Reached via in-language `import "..."` or yaml `imports:`.
    Imported,
}

pub struct Project {
    pub config: ProjectConfig,
    pub base_dir: PathBuf,
    pub programs: Vec<(String, Program)>,
    pub merged: Program,
    /// Catalog-link warnings collected before `merge_catalog_instantiations`.
    pub link_diags: Vec<crate::validate::Diagnostic>,
}

impl std::fmt::Debug for Project {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Project")
            .field("config", &self.config)
            .field("base_dir", &self.base_dir)
            .field(
                "sources",
                &self
                    .programs
                    .iter()
                    .map(|(s, _)| s.as_str())
                    .collect::<Vec<_>>(),
            )
            .field("entity_count", &self.merged.entities.len())
            .finish()
    }
}

#[derive(Debug)]
pub enum ProjectError {
    Io(String),
    Yaml(String),
    Parse(String),
    Merge(String),
    /// Packaging / import-layout errors (F4, F5). Not a textual merge conflict.
    Packaging {
        code: &'static str,
        message: String,
        path: String,
        span: Option<ast::Span>,
    },
    /// One or more parse diagnostics (possibly recovered, never an AST).
    Diagnostics(Vec<Diagnostic>),
}

impl std::fmt::Display for ProjectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProjectError::Io(msg) => write!(f, "IO error: {}", msg),
            ProjectError::Yaml(msg) => write!(f, "YAML error: {}", msg),
            ProjectError::Parse(msg) => write!(f, "Parse error: {}", msg),
            ProjectError::Merge(msg) => write!(f, "Merge conflict: {}", msg),
            ProjectError::Packaging { message, .. } => write!(f, "{message}"),
            ProjectError::Diagnostics(ds) => {
                for (i, d) in ds.iter().enumerate() {
                    if i > 0 {
                        writeln!(f)?;
                    }
                    write!(f, "{}", format_human(d))?;
                }
                Ok(())
            }
        }
    }
}

impl Project {
    pub fn load(yaml_path: &Path) -> Result<Project, ProjectError> {
        Self::load_limited(yaml_path, crate::diagnostic::DEFAULT_MAX_ERRORS)
    }

    pub fn load_limited(yaml_path: &Path, max_errors: usize) -> Result<Project, ProjectError> {
        let yaml_contents = std::fs::read_to_string(yaml_path)
            .map_err(|e| ProjectError::Io(format!("{}: {}", yaml_path.display(), e)))?;

        let mut config: ProjectConfig = serde_yml::from_str(&yaml_contents)
            .map_err(|e| ProjectError::Yaml(format!("{}", e)))?;
        config.key_warnings = config_key_warnings(&yaml_contents);

        let base_dir = yaml_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();

        if config.sources.is_empty() {
            return Err(ProjectError::Yaml("'sources' list is empty".to_string()));
        }

        // Parse every entry-point .cam file from `sources:`, then any
        // yaml `imports:` (library tier), then transitively walk
        // `import "..."` directives. Each program is tagged with its
        // `SourceKind` so merge can enforce F4 (no entity/test/fuzz/
        // invariant in imported files).
        let yaml_display = yaml_path.display().to_string();
        let library_roots = resolve_library_roots(&config.library_paths, &base_dir);
        let entries: Vec<PathBuf> = config.sources.iter().map(|s| base_dir.join(s)).collect();
        let extra_imported =
            resolve_yaml_imports(&config.imports, &yaml_display, &base_dir, &library_roots)?;
        let tagged = load_with_imports_in(
            &entries,
            &extra_imported,
            &base_dir,
            &library_roots,
            max_errors,
        )?;

        let programs: Vec<(String, Program)> = tagged
            .iter()
            .map(|(s, _k, p)| (s.clone(), p.clone()))
            .collect();
        let mut merged = merge_tagged_programs(&tagged)?;

        let pre_merge = crate::validate::catalog_pre_merge_check(&merged);
        if pre_merge
            .iter()
            .any(|d| d.severity == crate::validate::Severity::Error)
        {
            return Err(ProjectError::Diagnostics(pre_merge));
        }
        let link_diags = pre_merge
            .into_iter()
            .filter(|d| d.severity == crate::validate::Severity::Warning)
            .collect();
        crate::merge_catalog::merge_catalog_instantiations(&mut merged);

        // Lower `property` declarations into concrete `test` / `fuzz` decls
        // for every target except Lean, which reads `merged.properties`
        // directly so that sampling bounds never leak into its theorems.
        let target = config.parsed_target().map_err(ProjectError::Yaml)?;
        if target.desugars_properties() {
            crate::desugar::desugar_properties(&mut merged);
        }

        Ok(Project {
            config,
            base_dir,
            programs,
            merged,
            link_diags,
        })
    }

    /// Merged program from a project yaml **before** catalog link validation
    /// and property desugaring. Used by EVM compat corpus gates (V62/V63) on
    /// fixtures whose catalog tests may fail unrelated rules (e.g. T37 on
    /// `#[instantiates]` integration tests) while entity init routes are still
    /// valid.
    pub fn load_merged_pre_catalog(yaml_path: &Path) -> Result<Program, ProjectError> {
        Self::load_merged_pre_catalog_limited(
            yaml_path,
            crate::diagnostic::DEFAULT_MAX_ERRORS,
        )
    }

    pub fn load_merged_pre_catalog_limited(
        yaml_path: &Path,
        max_errors: usize,
    ) -> Result<Program, ProjectError> {
        let yaml_contents = std::fs::read_to_string(yaml_path)
            .map_err(|e| ProjectError::Io(format!("{}: {}", yaml_path.display(), e)))?;

        let mut config: ProjectConfig = serde_yml::from_str(&yaml_contents)
            .map_err(|e| ProjectError::Yaml(format!("{}", e)))?;
        config.key_warnings = config_key_warnings(&yaml_contents);

        let base_dir = yaml_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();

        if config.sources.is_empty() {
            return Err(ProjectError::Yaml("'sources' list is empty".to_string()));
        }

        let yaml_display = yaml_path.display().to_string();
        let library_roots = resolve_library_roots(&config.library_paths, &base_dir);
        let entries: Vec<PathBuf> = config.sources.iter().map(|s| base_dir.join(s)).collect();
        let extra_imported =
            resolve_yaml_imports(&config.imports, &yaml_display, &base_dir, &library_roots)?;
        let tagged = load_with_imports_in(
            &entries,
            &extra_imported,
            &base_dir,
            &library_roots,
            max_errors,
        )?;

        merge_tagged_programs(&tagged)
    }

    /// Single-file CLI mode: load `path` and walk its transitive
    /// `import "..."` directives. The entry file is `SourceKind::Entry`,
    /// every reached file is `SourceKind::Imported`.
    pub fn load_single_file(path: &Path) -> Result<Project, ProjectError> {
        let base_dir = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();

        let tagged = load_with_imports(&[path.to_path_buf()], &base_dir)?;

        let programs: Vec<(String, Program)> = tagged
            .iter()
            .map(|(s, _k, p)| (s.clone(), p.clone()))
            .collect();
        let merged = merge_tagged_programs(&tagged)?;

        let config = ProjectConfig {
            name: None,
            target: String::new(),
            output_dir: default_output_dir(),
            sources: vec![path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default()],
            library_paths: Vec::new(),
            imports: Vec::new(),
            deterministic_addresses: None,
            foundry: None,
            revm_tests: None,
            ackinacki: None,
            fuzz: None,
            invariant: None,
            lean: None,
            evm: None,
            key_warnings: Vec::new(),
        };

        Ok(Project {
            config,
            base_dir,
            programs,
            merged,
            link_diags: Vec::new(),
        })
    }

    /// Resolve the output directory relative to the YAML file's location.
    pub fn output_dir(&self) -> PathBuf {
        self.base_dir.join(&self.config.output_dir)
    }

    /// Project name: explicit config name, or derived from base directory.
    pub fn name(&self) -> String {
        self.config.name.clone().unwrap_or_else(|| {
            self.base_dir
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "project".to_string())
        })
    }
}

// ===========================================================================
// Multi-file import resolution
// ===========================================================================
//
// Phase Library-1: walks `import "..."` directives starting from a
// list of entry `.cam` files. Cycle detection keys off the canonical
// filesystem path. Library files (yaml `imports:` or anything reached
// only via `import`) are flagged `SourceKind::Imported` so the merge
// phase can reject `entity` / `test` / `fuzz` / `invariant` (F4).
// Bare (non-`./`/`../`) in-language imports also search `library_paths`
// after a file-relative miss (F5 if no root hits).

/// BFS-walks the transitive closure of `import` directives. Returns a
/// vector `(display_path, kind, parsed_program)` in load order. Cycle-
/// detection is on the *canonicalised* path so symlinks and
/// `./foo/../bar.cam` aliases collapse.
pub fn load_with_imports(
    entries: &[PathBuf],
    base_dir: &Path,
) -> Result<Vec<(String, SourceKind, Program)>, ProjectError> {
    load_with_imports_limited(entries, base_dir, crate::diagnostic::DEFAULT_MAX_ERRORS)
}

/// Like [`load_with_imports`], with an explicit parse-recovery budget.
pub fn load_with_imports_limited(
    entries: &[PathBuf],
    base_dir: &Path,
    max_errors: usize,
) -> Result<Vec<(String, SourceKind, Program)>, ProjectError> {
    load_with_imports_in(entries, &[], base_dir, &[], max_errors)
}

/// Like [`load_with_imports_limited`], plus yaml `imports:` (`extra_imported`,
/// already resolved) and `library_paths` roots for bare in-language specs.
/// `entries` are queued as [`SourceKind::Entry`] first so a path listed in
/// both `sources:` and `imports:` stays an entry (F4 does not fire).
pub fn load_with_imports_in(
    entries: &[PathBuf],
    extra_imported: &[PathBuf],
    base_dir: &Path,
    library_roots: &[PathBuf],
    max_errors: usize,
) -> Result<Vec<(String, SourceKind, Program)>, ProjectError> {
    crate::ast::reset_file_table();
    let mut out: Vec<(String, SourceKind, Program)> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut parse_diags: Vec<Diagnostic> = Vec::new();
    let mut queue: std::collections::VecDeque<(PathBuf, SourceKind)> =
        std::collections::VecDeque::new();
    for e in entries {
        queue.push_back((e.clone(), SourceKind::Entry));
    }
    for e in extra_imported {
        queue.push_back((e.clone(), SourceKind::Imported));
    }

    while let Some((path, kind)) = queue.pop_front() {
        let canon = canonicalise_or_pass(&path);
        if !seen.insert(canon.clone()) {
            continue;
        }

        let source = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                return Err(ProjectError::Io(format!("{}: {}", path.display(), e)));
            }
        };

        let rel = display_rel(&path, base_dir);
        let _guard = crate::ast::begin_parse_file(&rel, &source);
        match crate::ProgramParser::new().parse(&source) {
            Ok(program) => {
                let here_dir = path
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .to_path_buf();
                for fi in &program.file_imports {
                    let target = resolve_in_language_import(
                        &fi.path,
                        &here_dir,
                        library_roots,
                        &rel,
                        fi.span,
                    )?;
                    queue.push_back((target, SourceKind::Imported));
                }
                out.push((rel, kind, program));
            }
            Err(e) => {
                if parse_diags.len() < max_errors {
                    let recovered =
                        recover_parse_diagnostics(&source, &rel, max_errors - parse_diags.len());
                    if recovered.is_empty() {
                        parse_diags.push(diagnostic_from_parse_error(&e, &rel, &source, 0));
                    } else {
                        parse_diags.extend(recovered);
                    }
                }
            }
        }
    }

    if !parse_diags.is_empty() {
        parse_diags.truncate(max_errors);
        return Err(ProjectError::Diagnostics(parse_diags));
    }

    Ok(out)
}

fn canonicalise_or_pass(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

fn display_rel(p: &Path, base_dir: &Path) -> String {
    p.strip_prefix(base_dir)
        .map(|rel| rel.to_string_lossy().to_string())
        .unwrap_or_else(|_| p.to_string_lossy().to_string())
}

fn f4(path: &str, span: ast::Span, message: String) -> ProjectError {
    ProjectError::Packaging {
        code: "F4",
        message,
        path: path.to_string(),
        span: Some(span),
    }
}

fn f5(path: &str, span: Option<ast::Span>, spec: &str, probed: &[PathBuf]) -> ProjectError {
    let probed_s = probed
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    ProjectError::Packaging {
        code: "F5",
        message: format!("F5: import '{spec}' did not resolve (probed: {probed_s})"),
        path: path.to_string(),
        span,
    }
}

/// Expand `${VAR}` from the process environment. Unset variables (and
/// empty names) stay as the literal `${VAR}` so they cannot silently
/// collapse to `base_dir`.
pub(crate) fn expand_env_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find('}') {
            Some(end) => {
                let name = &after[..end];
                match std::env::var(name) {
                    Ok(val) if !name.is_empty() => out.push_str(&val),
                    _ => {
                        out.push_str("${");
                        out.push_str(name);
                        out.push('}');
                    }
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

fn is_explicitly_relative(spec: &str) -> bool {
    spec.starts_with("./")
        || spec.starts_with("../")
        || spec.starts_with(".\\")
        || spec.starts_with("..\\")
}

fn resolve_library_roots(specs: &[String], base_dir: &Path) -> Vec<PathBuf> {
    specs
        .iter()
        .filter(|s| !s.is_empty())
        .map(|s| {
            let expanded = expand_env_path(s);
            let p = PathBuf::from(expanded);
            if p.is_absolute() {
                p
            } else {
                base_dir.join(p)
            }
        })
        .collect()
}

fn resolve_yaml_imports(
    specs: &[String],
    yaml_display: &str,
    base_dir: &Path,
    library_roots: &[PathBuf],
) -> Result<Vec<PathBuf>, ProjectError> {
    specs
        .iter()
        .map(|spec| resolve_search_spec(spec, yaml_display, None, base_dir, library_roots))
        .collect()
}

fn resolve_search_spec(
    spec: &str,
    err_path: &str,
    span: Option<ast::Span>,
    first_root: &Path,
    library_roots: &[PathBuf],
) -> Result<PathBuf, ProjectError> {
    let expanded = expand_env_path(spec);
    let p = Path::new(&expanded);
    let mut probed = Vec::new();
    if p.is_absolute() {
        probed.push(p.to_path_buf());
        if p.is_file() {
            return Ok(p.to_path_buf());
        }
        return Err(f5(err_path, span, spec, &probed));
    }
    let first = first_root.join(p);
    probed.push(first.clone());
    if first.is_file() {
        return Ok(first);
    }
    for root in library_roots {
        let cand = root.join(p);
        probed.push(cand.clone());
        if cand.is_file() {
            return Ok(cand);
        }
    }
    Err(f5(err_path, span, spec, &probed))
}

fn resolve_in_language_import(
    spec: &str,
    here_dir: &Path,
    library_roots: &[PathBuf],
    importer_display: &str,
    span: ast::Span,
) -> Result<PathBuf, ProjectError> {
    if is_explicitly_relative(spec) {
        return Ok(here_dir.join(spec));
    }
    resolve_search_spec(spec, importer_display, Some(span), here_dir, library_roots)
}

// ===========================================================================
// Program merging
// ===========================================================================

/// Public CLI helper — single-file mode merges via the same path.
pub fn merge_tagged_for_single_file(
    programs: &[(String, SourceKind, Program)],
) -> Result<Program, ProjectError> {
    merge_tagged_programs(programs)
}

/// Phase Library-1: rejects `entity` / `test` / `fuzz` / `invariant`
/// declarations sourced from imported files (F4). Otherwise merges
/// declarations exactly as `merge_programs` did pre-Library-1.
fn merge_tagged_programs(
    programs: &[(String, SourceKind, Program)],
) -> Result<Program, ProjectError> {
    // Reject library-file declarations of entity/test/fuzz/invariant
    // before delegating to the (otherwise unchanged) flat merge.
    for (source, kind, program) in programs {
        if *kind != SourceKind::Imported {
            continue;
        }
        if let Some(e) = program.entities.first() {
            return Err(f4(
                source,
                e.span,
                format!(
                    "F4: imported file '{}' declares entity '{}' \
                     (only declarations may appear in imported files; \
                     list this file under `sources:` in project.yaml \
                     to make it a project entry instead)",
                    source, e.name,
                ),
            ));
        }
        if let Some(t) = program.tests.first() {
            return Err(f4(
                source,
                t.span,
                format!(
                    "F4: imported file '{}' declares test \"{}\" \
                     (tests must live in entry files, not imported libraries)",
                    source, t.name,
                ),
            ));
        }
        if let Some(f) = program.fuzz_tests.first() {
            return Err(f4(
                source,
                f.span,
                format!(
                    "F4: imported file '{}' declares fuzz \"{}\" \
                     (fuzz tests must live in entry files, not imported libraries)",
                    source, f.name,
                ),
            ));
        }
        if let Some(i) = program.invariants.first() {
            return Err(f4(
                source,
                i.span,
                format!(
                    "F4: imported file '{}' declares invariant \"{}\" \
                     (invariants must live in entry files, not imported libraries)",
                    source, i.name,
                ),
            ));
        }
    }

    let flat: Vec<(String, Program)> = programs
        .iter()
        .map(|(s, _k, p)| (s.clone(), p.clone()))
        .collect();
    merge_programs(&flat)
}

fn merge_programs(programs: &[(String, Program)]) -> Result<Program, ProjectError> {
    let mut merged = Program {
        imports: vec![],
        file_imports: vec![],
        pure_fns: vec![],
        type_aliases: vec![],
        records: vec![],
        enums: vec![],
        entities: vec![],
        extern_entities: vec![],
        tests: vec![],
        properties: vec![],
        fuzz_tests: vec![],
        invariants: vec![],
        events: vec![],
        errors: vec![],
        libraries: vec![],
        using_decls: vec![],
    };

    let mut seen_imports: HashSet<String> = HashSet::new();
    let mut seen_entities: HashSet<String> = HashSet::new();
    let mut seen_records: HashSet<String> = HashSet::new();
    let mut seen_enums: HashSet<String> = HashSet::new();
    let mut seen_aliases: HashSet<String> = HashSet::new();
    let mut seen_fns: HashSet<String> = HashSet::new();
    let mut seen_externs: HashSet<String> = HashSet::new();
    let mut seen_libraries: HashSet<String> = HashSet::new();
    let mut seen_events: HashSet<String> = HashSet::new();
    let mut seen_errors: HashSet<String> = HashSet::new();
    let mut seen_file_imports: HashSet<String> = HashSet::new();

    for (source, program) in programs {
        for import in &program.imports {
            if seen_imports.insert(import.namespace.clone()) {
                merged.imports.push(import.clone());
            }
        }

        for entity in &program.entities {
            if !seen_entities.insert(entity.name.clone()) {
                return Err(ProjectError::Merge(format!(
                    "duplicate entity '{}' (first defined before {}, redefined in {})",
                    entity.name,
                    find_first_source(
                        programs,
                        |p| p.entities.iter().any(|e| e.name == entity.name),
                        source
                    ),
                    source
                )));
            }
            merged.entities.push(entity.clone());
        }

        for rec in &program.records {
            if !seen_records.insert(rec.name.clone()) {
                return Err(ProjectError::Merge(format!(
                    "duplicate record '{}' in {}",
                    rec.name, source
                )));
            }
            merged.records.push(rec.clone());
        }

        for en in &program.enums {
            if !seen_enums.insert(en.name.clone()) {
                return Err(ProjectError::Merge(format!(
                    "duplicate enum '{}' in {}",
                    en.name, source
                )));
            }
            merged.enums.push(en.clone());
        }

        for ta in &program.type_aliases {
            if !seen_aliases.insert(ta.name.clone()) {
                return Err(ProjectError::Merge(format!(
                    "duplicate type alias '{}' in {}",
                    ta.name, source
                )));
            }
            merged.type_aliases.push(ta.clone());
        }

        for f in &program.pure_fns {
            if !seen_fns.insert(f.name.clone()) {
                return Err(ProjectError::Merge(format!(
                    "duplicate pure function '{}' in {}",
                    f.name, source
                )));
            }
            merged.pure_fns.push(f.clone());
        }

        for ext in &program.extern_entities {
            if !seen_externs.insert(ext.name.clone()) {
                return Err(ProjectError::Merge(format!(
                    "duplicate extern entity '{}' in {}",
                    ext.name, source
                )));
            }
            merged.extern_entities.push(ext.clone());
        }

        for lib in &program.libraries {
            if !seen_libraries.insert(lib.name.clone()) {
                return Err(ProjectError::Merge(format!(
                    "duplicate library '{}' in {}",
                    lib.name, source
                )));
            }
            merged.libraries.push(lib.clone());
        }

        for ev in &program.events {
            if !seen_events.insert(ev.name.clone()) {
                return Err(ProjectError::Merge(format!(
                    "duplicate event '{}' in {}",
                    ev.name, source
                )));
            }
            merged.events.push(ev.clone());
        }

        for er in &program.errors {
            if !seen_errors.insert(er.name.clone()) {
                return Err(ProjectError::Merge(format!(
                    "duplicate error '{}' in {}",
                    er.name, source
                )));
            }
            merged.errors.push(er.clone());
        }

        for fi in &program.file_imports {
            if seen_file_imports.insert(fi.path.clone()) {
                merged.file_imports.push(fi.clone());
            }
        }

        // `using` decls are not name-collision checked here — the
        // validator (V55/V56/V57) handles semantic conflicts.
        merged.using_decls.extend(program.using_decls.clone());

        merged.tests.extend(program.tests.clone());
        merged.properties.extend(program.properties.clone());
        merged.fuzz_tests.extend(program.fuzz_tests.clone());
        merged.invariants.extend(program.invariants.clone());
    }

    ast::normalize_program_types(&mut merged);

    // Phase Library-2: rewrite `recv.method(args)` -> `method(recv, args)`
    // for every (method, receiver_type) covered by an in-scope
    // `using ... for T;` directive. Pure AST pass; codegen sees only
    // the rewritten form.
    crate::using_rewrite::apply_using_rewrites(&mut merged);

    Ok(merged)
}

fn find_first_source<F>(programs: &[(String, Program)], predicate: F, exclude: &str) -> String
where
    F: Fn(&Program) -> bool,
{
    for (src, prog) in programs {
        if src != exclude && predicate(prog) {
            return src.clone();
        }
    }
    "unknown".to_string()
}

// ===========================================================================
// Helper: reorder entities so a given entity is first
// ===========================================================================

/// Create a clone of the program with the named entity moved to the front.
/// Used by per-entity backends that rely on `program.entities.first()`.
pub fn program_with_entity_first(program: &Program, entity_name: &str) -> Program {
    let mut reordered = program.clone();
    if let Some(pos) = reordered
        .entities
        .iter()
        .position(|e| e.name == entity_name)
    {
        let entity = reordered.entities.remove(pos);
        reordered.entities.insert(0, entity);
    }
    reordered
}

#[cfg(test)]
mod expand_env_tests {
    use super::expand_env_path;

    #[test]
    fn leaves_unset_var_literal() {
        let out = expand_env_path("${CAMBRIAN_SURELY_UNSET_XYZ_98765}/lib");
        assert_eq!(out, "${CAMBRIAN_SURELY_UNSET_XYZ_98765}/lib");
    }

    #[test]
    fn expands_set_var() {
        let key = format!(
            "CAMBRIAN_TEST_EXPAND_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        std::env::set_var(&key, "/opt/cam-libs");
        let spec = format!("${{{key}}}/token");
        assert_eq!(expand_env_path(&spec), "/opt/cam-libs/token");
        std::env::remove_var(&key);
    }

    #[test]
    fn empty_name_stays_literal() {
        assert_eq!(expand_env_path("${}/x"), "${}/x");
    }
}
