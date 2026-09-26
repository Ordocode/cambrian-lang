// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{self, Command};

use cambrian_transpiler::codegen::{self, EvmSolidityBackend, LeanBackend, OutputBackend};
#[cfg(feature = "rust-targets")]
use cambrian_transpiler::codegen::{
    AckiNackiAdapter, AckiNackiBackend, AckiNackiWasmHost, ContainerAdapter, NativeHost,
    RustBackend, WasmHost,
};
use cambrian_transpiler::diagnostic::{
    build_identity_json, has_errors, print_diagnostics_default, DiagFormat, DEFAULT_MAX_ERRORS,
};
use cambrian_transpiler::project::{Project, ProjectError};
use cambrian_transpiler::target::Target;
use cambrian_transpiler::validate::{self, Diagnostic, Phase};

struct Cli {
    project: Option<String>,
    dump_ast: bool,
    source_map: bool,
    target: Option<String>,
    output: Option<String>,
    check_lean: bool,
    check: bool,
    diagnostic_format: DiagFormat,
    max_errors: usize,
    version: bool,
    version_json: bool,
    help: bool,
    rest: Vec<String>,
}

fn usage() {
    eprintln!(
        "Usage: cambrian-transpiler <input.cam> [-o <output_dir>] [--dump-ast] [--source-map]"
    );
    eprintln!(
        "                           [--target {}] [--check-lean]",
        Target::expected_names()
    );
    eprintln!(
        "                           [--check] [--diagnostic-format human|json] [--max-errors N]"
    );
    eprintln!("       cambrian-transpiler --project <project.yaml> [--check] [--check-lean]");
    eprintln!("       cambrian-transpiler --version [--json]");
    eprintln!("       cambrian-transpiler --help");
    eprintln!("  Transpiles .cam files into buildable source code.");
    eprintln!("  --project              Compile a multi-file project from a YAML config.");
    eprintln!("  --dump-ast             Print parsed AST in readable Cambrian format and exit.");
    eprintln!("  --source-map           Emit .cam.map JSON alongside generated code.");
    eprintln!(
        "  --target               Compilation target: {} (default {}).",
        Target::expected_names(),
        Target::default_cli().name()
    );
    eprintln!(
        "  --check                Parse + validate + target-compat; do not write generated files."
    );
    eprintln!(
        "                         Catalog #[instantiates] links merge in --project mode; delta-only"
    );
    eprintln!(
        "                         supplemental .cam files need --project <project.yaml> --check."
    );
    eprintln!("  --check-lean           After emitting a Lean project, run `lake build` in the output dir.");
    eprintln!("  --diagnostic-format    human (default) or json.");
    eprintln!(
        "  --max-errors N         Cap recovered parse diagnostics (default {DEFAULT_MAX_ERRORS})."
    );
    eprintln!("  --version [--json]     Print compiler build identity.");
    eprintln!("  Default output dir: build/<entity_name>-entity/");
}

fn parse_cli(args: &[String]) -> Cli {
    let mut cli = Cli {
        project: None,
        dump_ast: false,
        source_map: false,
        target: None,
        output: None,
        check_lean: false,
        check: false,
        diagnostic_format: DiagFormat::Human,
        max_errors: DEFAULT_MAX_ERRORS,
        version: false,
        version_json: false,
        help: false,
        rest: Vec::new(),
    };
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => cli.help = true,
            "--version" => cli.version = true,
            "--json" => cli.version_json = true,
            "--dump-ast" => cli.dump_ast = true,
            "--source-map" => cli.source_map = true,
            "--check-lean" => cli.check_lean = true,
            "--check" => cli.check = true,
            "--project" => {
                i += 1;
                cli.project = Some(args.get(i).cloned().unwrap_or_default());
            }
            "--target" => {
                i += 1;
                cli.target = Some(args.get(i).cloned().unwrap_or_default());
            }
            "-o" => {
                i += 1;
                cli.output = Some(args.get(i).cloned().unwrap_or_default());
            }
            "--diagnostic-format" => {
                i += 1;
                match args.get(i).map(|s| s.as_str()) {
                    Some("json") => cli.diagnostic_format = DiagFormat::Json,
                    Some("human") => cli.diagnostic_format = DiagFormat::Human,
                    other => {
                        eprintln!(
                            "Error: --diagnostic-format expects human|json, got {:?}",
                            other
                        );
                        process::exit(1);
                    }
                }
            }
            "--max-errors" => {
                i += 1;
                let raw = args.get(i).cloned().unwrap_or_default();
                match raw.parse::<usize>() {
                    Ok(n) if n > 0 => cli.max_errors = n,
                    _ => {
                        eprintln!("Error: --max-errors requires a positive integer");
                        process::exit(1);
                    }
                }
            }
            s if s.starts_with('-') => {
                eprintln!("Error: unknown flag {s}");
                usage();
                process::exit(1);
            }
            s => cli.rest.push(s.to_string()),
        }
        i += 1;
    }
    cli
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let cli = parse_cli(&args);

    if cli.help {
        usage();
        process::exit(0);
    }
    if cli.version {
        if cli.version_json || cli.diagnostic_format == DiagFormat::Json {
            println!("{}", build_identity_json());
        } else {
            let id = build_identity_json();
            println!(
                "cambrian-transpiler {} ({})",
                id["version"].as_str().unwrap_or("0.1.0"),
                id["gitCommit"].as_str().unwrap_or("unknown")
            );
            if id["dirty"].as_bool() == Some(true) {
                println!("dirty: true");
            }
        }
        return;
    }

    if args.len() < 2 {
        usage();
        process::exit(1);
    }

    if let Some(yaml) = &cli.project {
        if yaml.is_empty() {
            eprintln!("Error: --project requires a YAML file path");
            process::exit(1);
        }
        run_project(yaml, &cli);
        return;
    }

    run_single_file(&cli);
}

fn project_error_to_diags(e: ProjectError) -> Vec<Diagnostic> {
    match e {
        ProjectError::Diagnostics(ds) => ds,
        ProjectError::Packaging {
            code,
            message,
            path,
            span,
        } => {
            let mut d = Diagnostic::error(code, message);
            d.phase = Phase::Project;
            if let Some(sp) = span {
                d = d.with_span(sp);
            }
            d.path = Some(path);
            vec![d]
        }
        ProjectError::Parse(msg) => {
            let mut d = Diagnostic::error("PARSE_UNEXPECTED_TOKEN", format!("Parse error: {msg}"));
            d.phase = Phase::Parse;
            vec![d]
        }
        ProjectError::Io(msg) => {
            let mut d = Diagnostic::error("PKG_IO", format!("IO error: {msg}"));
            d.phase = Phase::Project;
            vec![d]
        }
        ProjectError::Yaml(msg) => {
            let mut d = Diagnostic::error("PKG_YAML", format!("YAML error: {msg}"));
            d.phase = Phase::Project;
            vec![d]
        }
        ProjectError::Merge(msg) => {
            let mut d = Diagnostic::error("PKG_MERGE", format!("Merge conflict: {msg}"));
            d.phase = Phase::Project;
            vec![d]
        }
    }
}

fn program_has_instantiates_links(program: &cambrian_transpiler::ast::Program) -> bool {
    program.tests.iter().any(|t| t.instantiates.is_some())
        || program.fuzz_tests.iter().any(|f| f.instantiates.is_some())
        || program.properties.iter().any(|p| p.instantiates.is_some())
        || program.invariants.iter().any(|i| i.instantiates.is_some())
}

fn maybe_emit_supplemental_check_hint(diags: &[Diagnostic], has_links: bool) {
    if !has_links {
        return;
    }
    if diags.iter().any(|d| d.code == "T6" || d.code == "I5") {
        eprintln!(
            "hint: supplemental #[instantiates] files validate only after catalog merge; \
             use cambrian-transpiler --project <project.yaml> --check"
        );
    }
}

fn emit_and_maybe_exit(diags: &[Diagnostic], format: DiagFormat) {
    print_diagnostics_default(diags, format);
    if has_errors(diags) {
        process::exit(1);
    }
}

fn emit_nonfatal(diagnostics: &[Diagnostic], format: DiagFormat) {
    if diagnostics.is_empty() {
        return;
    }
    print_diagnostics_default(diagnostics, format);
}

/// E27 — refuse to report success when codegen replaced an expression it could
/// not lower with the literal `0`.
///
/// Called after the files are written: the artifact is the evidence a fix needs,
/// and the non-zero exit is what keeps it from being treated as a build. Without
/// this the CLI exited 0 on a `pure fn` whose whole body had become `return 0;`.
fn reject_unlowered_values(files: &[(String, String)]) {
    let found = cambrian_transpiler::codegen::find_unlowered_values(files);
    if !found.is_empty() {
        eprint!(
            "{}",
            cambrian_transpiler::codegen::unlowered_value_report(&found)
        );
        process::exit(1);
    }
}

fn emit_project_files(files: &[(String, String)], output_dir: &Path, quiet: bool) {
    for (rel_path, contents) in files {
        let path = output_dir.join(rel_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap_or_else(|e| {
                eprintln!("Error creating {}: {}", parent.display(), e);
                process::exit(1);
            });
        }
        fs::write(&path, contents).unwrap_or_else(|e| {
            eprintln!("Error writing {}: {}", path.display(), e);
            process::exit(1);
        });
        if !quiet {
            println!("  wrote {}", path.display());
        }
    }
}

// ===========================================================================
// Project mode: --project <path.yaml>
// ===========================================================================

/// Run `lake build` inside `output_dir`. Aborts the process on failure
/// (mirrors how `--source-map` and the EVM target propagate errors).
fn run_lake_build(output_dir: &Path) {
    println!("  Running `lake build` in {}", output_dir.display());
    let status = Command::new("lake")
        .arg("build")
        .current_dir(output_dir)
        .status();
    match status {
        Ok(s) if s.success() => {
            println!("  Lean build OK");
        }
        Ok(s) => {
            eprintln!(
                "Error: `lake build` failed with status {} in {}",
                s,
                output_dir.display()
            );
            process::exit(1);
        }
        Err(e) => {
            eprintln!("Error: failed to invoke `lake` (is it on PATH?): {}", e);
            process::exit(1);
        }
    }
}

fn cli_warning(cli: &Cli, msg: &str) {
    if cli.diagnostic_format != DiagFormat::Json {
        eprintln!("warning: {msg}");
    }
}

fn run_project(yaml_path: &str, cli: &Cli) {
    let yaml = Path::new(yaml_path);
    for (set, flag, why) in [
        (cli.output.is_some(), "-o", "the output dir comes from `output_dir` in project.yaml"),
        (cli.target.is_some(), "--target", "the target comes from `target` in project.yaml"),
        (cli.dump_ast, "--dump-ast", "it only applies to single-file mode"),
        (cli.source_map, "--source-map", "it only applies to single-file mode"),
    ] {
        if set {
            cli_warning(cli, &format!("{flag} is ignored with --project ({why})"));
        }
    }
    let project = match Project::load_limited(yaml, cli.max_errors) {
        Ok(p) => p,
        Err(e) => {
            let diags = project_error_to_diags(e);
            emit_and_maybe_exit(&diags, cli.diagnostic_format);
            process::exit(1);
        }
    };

    let target = match project.config.parsed_target() {
        Ok(t) => t,
        Err(e) => {
            let mut d = Diagnostic::error("PKG_YAML", e);
            d.phase = Phase::Project;
            emit_and_maybe_exit(&[d], cli.diagnostic_format);
            process::exit(1);
        }
    };

    let mut diagnostics = project.link_diags.clone();
    diagnostics.extend(validate::validate(&project.merged));
    diagnostics.extend(validate::validate_project_config(&project.config));
    // B7 (CAMBRIAN_R3_SPEC §7): predictable-profile name validator. The
    // `predictable` flag gates the zone/collision rules; the `__cbr_`
    // prefix reservation is unconditional. Legacy projects only ever see
    // the unconditional rule.
    let predictable_profile = codegen::lean::lean_use_predictable_profile(&project.config);
    diagnostics.extend(validate::check_reserved_names(
        &project.merged,
        predictable_profile,
    ));
    if has_errors(&diagnostics) {
        emit_and_maybe_exit(&diagnostics, cli.diagnostic_format);
        return;
    }

    let det = project.config.resolved_deterministic_addresses();
    let mut compat = validate::check_target_compat(&project.merged, target, det);
    for d in &mut compat {
        d.phase = Phase::Target;
    }
    diagnostics.extend(compat);
    if has_errors(&diagnostics) {
        emit_and_maybe_exit(&diagnostics, cli.diagnostic_format);
        return;
    }

    if cli.check {
        emit_and_maybe_exit(&diagnostics, cli.diagnostic_format);
        return;
    }

    emit_nonfatal(&diagnostics, cli.diagnostic_format);

    // `property` declarations were already desugared into concrete
    // `test` / `fuzz` decls by `Project::load` (skipped for the Lean
    // target), so the backend sees the lowered fixtures directly.
    let output_dir = project.output_dir();
    let backend: Box<dyn OutputBackend> = make_backend(
        target,
        &output_dir,
        project.config.resolved_lean().lean_toolchain.clone(),
    );

    let files = backend.gen_project(&project);
    let quiet = cli.diagnostic_format == DiagFormat::Json;
    emit_project_files(&files, &output_dir, quiet);
    if target == Target::Evm {
        cambrian_transpiler::manifest::write_evm_manifest(&output_dir, &files).unwrap_or_else(
            |e| {
                eprintln!(
                    "Error writing {}: {}",
                    cambrian_transpiler::manifest::MANIFEST_REL_PATH,
                    e
                );
                process::exit(1);
            },
        );
    }
    reject_unlowered_values(&files);

    if !quiet {
        let entity_count = project.merged.entities.len();
        let source_count = project.config.sources.len();
        println!(
            "Transpiled project '{}' ({} source files, {} entities) -> {} target",
            project.name(),
            source_count,
            entity_count,
            backend.target_description(),
        );
        println!("  Output: {}", output_dir.display());
    }

    // The overlay note names a module that exists only with the private
    // `plausible` feature. The public snapshot builds with that feature off.
    #[cfg(feature = "plausible")]
    if target == Target::Lean
        && codegen::lean::evm::plausible::lean_emit_plausible_overlay(&project.config)
    {
        let generated = output_dir.join("plausible/Testing/CambrianGenerated.lean");
        if !generated.exists() && !quiet {
            eprintln!(
                "note: plausible overlay under {}/plausible/ — run `plausible-pipeline run-all` (cambrian-gen) before `lake build runner` (see plausible/README-bootstrap.md)",
                output_dir.display()
            );
        }
    }

    if cli.check_lean {
        if target == Target::Lean {
            run_lake_build(&output_dir);
        } else if !quiet {
            eprintln!(
                "warning: --check-lean is only meaningful for --target lean (target was {})",
                target.name(),
            );
        }
    }
}

/// `lean_toolchain` is the project's `lean.lean_toolchain` override, or
/// `None` for the transpiler's default pin. Single-file mode always passes
/// `None` — there is no project file to read it from.
fn make_backend(
    target: Target,
    output_dir: &Path,
    lean_toolchain: Option<String>,
) -> Box<dyn OutputBackend> {
    match target {
        Target::Evm => Box::new(EvmSolidityBackend {
            deterministic_addresses: true,
        }),
        Target::Lean => Box::new(match lean_toolchain {
            Some(pin) => LeanBackend {
                lean_toolchain: pin,
                ..LeanBackend::default()
            },
            None => LeanBackend::default(),
        }),
        #[cfg(feature = "rust-targets")]
        Target::AckiNacki | Target::Wasm | Target::Native => make_rust_backend(target, output_dir),
        #[cfg(not(feature = "rust-targets"))]
        Target::AckiNacki | Target::Wasm | Target::Native => {
            unreachable!("rejected by Target::from_name")
        }
    }
}

#[cfg(feature = "rust-targets")]
include!("make_backend_rust.rs");

// ===========================================================================
// Single-file mode (original behaviour)
// ===========================================================================

fn run_single_file(cli: &Cli) {
    let dump_ast = cli.dump_ast;
    let emit_source_map = cli.source_map;
    let quiet = cli.diagnostic_format == DiagFormat::Json;

    let target = match &cli.target {
        Some(name) if !name.is_empty() => Target::from_name(name).unwrap_or_else(|| {
            eprintln!(
                "Error: unknown target '{}', expected {}",
                name,
                Target::expected_names()
            );
            process::exit(1);
        }),
        Some(_) => {
            eprintln!(
                "Error: --target requires an argument ({})",
                Target::expected_names()
            );
            process::exit(1);
        }
        None => Target::default_cli(),
    };

    if emit_source_map && matches!(target, Target::Evm | Target::Lean) {
        cli_warning(
            cli,
            &format!(
                "--source-map is only implemented for Rust targets; ignored for --target {}",
                target.name()
            ),
        );
    }
    if cli.check_lean && target != Target::Lean {
        cli_warning(
            cli,
            &format!(
                "--check-lean is only meaningful for --target lean (target was {})",
                target.name()
            ),
        );
    }

    let input_files: Vec<&String> = cli.rest.iter().filter(|a| a.ends_with(".cam")).collect();
    if input_files.is_empty() {
        eprintln!("Error: missing input .cam file");
        usage();
        process::exit(1);
    }
    let input_files: Vec<&str> = input_files.iter().map(|s| s.as_str()).collect();
    let input_path = input_files[0];
    if !Path::new(input_path).exists() {
        let mut d = Diagnostic::error("PKG_IO", format!("file not found: {input_path}"));
        d.phase = Phase::Project;
        d.path = Some(input_path.to_string());
        emit_and_maybe_exit(&[d], cli.diagnostic_format);
        process::exit(1);
    }

    let entry_paths: Vec<PathBuf> = input_files.iter().map(PathBuf::from).collect();
    let base_dir = entry_paths
        .first()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    let tagged = match cambrian_transpiler::project::load_with_imports_limited(
        &entry_paths,
        &base_dir,
        cli.max_errors,
    ) {
        Ok(t) => t,
        Err(e) => {
            emit_and_maybe_exit(&project_error_to_diags(e), cli.diagnostic_format);
            process::exit(1);
        }
    };

    let mut program = match cambrian_transpiler::project::merge_tagged_for_single_file(&tagged) {
        Ok(p) => p,
        Err(e) => {
            emit_and_maybe_exit(&project_error_to_diags(e), cli.diagnostic_format);
            process::exit(1);
        }
    };

    let source: String = entry_paths
        .iter()
        .map(|p| fs::read_to_string(p).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");

    cambrian_transpiler::ast::normalize_program_types(&mut program);
    cambrian_transpiler::using_rewrite::apply_using_rewrites(&mut program);

    if dump_ast {
        print!("{}", cambrian_transpiler::pretty::pretty_print(&program));
        return;
    }

    let mut diagnostics = validate::validate(&program);
    diagnostics.extend(validate::check_reserved_names(&program, false));
    let has_links = program_has_instantiates_links(&program);
    if has_errors(&diagnostics) {
        maybe_emit_supplemental_check_hint(&diagnostics, has_links);
        emit_and_maybe_exit(&diagnostics, cli.diagnostic_format);
        return;
    }

    let entity_name = program
        .entities
        .first()
        .map(|e| e.name.clone())
        .unwrap_or_else(|| {
            Path::new(input_path)
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        });

    let output_dir = if let Some(dir) = &cli.output {
        if dir.is_empty() {
            eprintln!("Error: -o requires an argument");
            process::exit(1);
        }
        PathBuf::from(dir)
    } else {
        PathBuf::from(format!("build/{}-entity", snake_case(&entity_name)))
    };

    // The EVM backend always emits the CREATE2 factory, so validate the
    // same deployment model as a project with `deterministic_addresses: true`.
    let mut compat = validate::check_target_compat(&program, target, target == Target::Evm);
    for d in &mut compat {
        d.phase = Phase::Target;
        if matches!(d.code, "V62" | "V63") && d.severity == validate::Severity::Error {
            d.severity = validate::Severity::Warning;
            d.message
                .push_str(" (a warning in single-file mode; an error with `--project`)");
        }
    }
    diagnostics.extend(compat);
    if has_errors(&diagnostics) {
        maybe_emit_supplemental_check_hint(&diagnostics, has_links);
        emit_and_maybe_exit(&diagnostics, cli.diagnostic_format);
        return;
    }

    if cli.check {
        emit_and_maybe_exit(&diagnostics, cli.diagnostic_format);
        return;
    }

    emit_nonfatal(&diagnostics, cli.diagnostic_format);

    cambrian_transpiler::merge_catalog::merge_catalog_instantiations(&mut program);

    if target.desugars_properties() {
        cambrian_transpiler::desugar::desugar_properties(&mut program);
    }

    let backend: Box<dyn OutputBackend> = make_backend(target, &output_dir, None);

    if target == Target::Lean {
        emit_lean_single(
            &*backend,
            &program,
            &entity_name,
            &output_dir,
            input_path,
            cli.check_lean,
            quiet,
        );
        return;
    }

    emit(
        &*backend,
        &program,
        &entity_name,
        &output_dir,
        input_path,
        emit_source_map,
        &source,
        target,
        quiet,
    );
}

/// Lean single-file mode: emit the per-entity Lean file plus the Lake
/// scaffolding files. Optionally runs `lake build` when `--check-lean`
/// is set.
fn emit_lean_single(
    backend: &dyn OutputBackend,
    program: &cambrian_transpiler::ast::Program,
    entity_name: &str,
    output_dir: &Path,
    input_path: &str,
    check_lean: bool,
    quiet: bool,
) {
    let mut files: Vec<(String, String)> = Vec::new();
    files.push((
        backend.file_name_for_entity(entity_name),
        backend.gen_program(program),
    ));
    files.extend(backend.extra_files(program, entity_name));
    emit_project_files(&files, output_dir, quiet);

    if !quiet {
        println!(
            "Transpiled {} -> {} target",
            input_path,
            backend.target_description()
        );
        println!("  Output: {}", output_dir.display());
        println!("  Entity: {}", entity_name);
        println!("\nTo build: cd {} && lake build", output_dir.display());
    }

    if check_lean {
        run_lake_build(output_dir);
    }
}

fn emit(
    backend: &dyn OutputBackend,
    program: &cambrian_transpiler::ast::Program,
    entity_name: &str,
    output_dir: &Path,
    input_path: &str,
    emit_source_map: bool,
    source: &str,
    target: Target,
    quiet: bool,
) {
    let is_rust = backend.file_extension() == "rs";

    let (code, source_map_builder) = if emit_source_map && is_rust {
        #[cfg(feature = "rust-targets")]
        {
            let source_opt = Some(source);
            let (mut mapped_code, smb) = match target {
                Target::AckiNacki => codegen::generate_mapped(
                    program,
                    &AckiNackiAdapter,
                    &AckiNackiWasmHost,
                    source_opt,
                ),
                Target::Wasm => {
                    codegen::generate_mapped(program, &ContainerAdapter, &WasmHost, source_opt)
                }
                _ => codegen::generate_mapped(program, &ContainerAdapter, &NativeHost, source_opt),
            };
            if target == Target::AckiNacki {
                let entity = program.entities.first().unwrap();
                mapped_code.push_str(&AckiNackiWasmHost::gen_action_constants(entity));
                mapped_code.push('\n');
                mapped_code.push_str(&AckiNackiWasmHost::gen_execute_dispatch(entity));
                mapped_code.push_str(&AckiNackiWasmHost::gen_wit_glue());
            }
            (mapped_code, Some(smb))
        }
        #[cfg(not(feature = "rust-targets"))]
        {
            let _ = (target, source);
            (
                backend.gen_program(program),
                None::<cambrian_transpiler::sourcemap::SourceMapBuilder>,
            )
        }
    } else {
        (
            backend.gen_program(program),
            None::<cambrian_transpiler::sourcemap::SourceMapBuilder>,
        )
    };

    let file_name = backend.file_name_for_entity(entity_name);
    let output_file = output_dir.join(&file_name);
    let mut emitted_files: Vec<(String, String)> = Vec::new();

    if let Some(parent) = output_file.parent() {
        fs::create_dir_all(parent).unwrap_or_else(|e| {
            eprintln!("Error creating {}: {}", parent.display(), e);
            process::exit(1);
        });
    }

    fs::write(&output_file, &code).unwrap_or_else(|e| {
        eprintln!("Error writing {}: {}", output_file.display(), e);
        process::exit(1);
    });
    emitted_files.push((file_name.clone(), code.clone()));
    reject_unlowered_values(&[(file_name.clone(), code.clone())]);

    for (rel_path, contents) in backend.extra_files(program, entity_name) {
        let path = output_dir.join(&rel_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap_or_else(|e| {
                eprintln!("Error creating {}: {}", parent.display(), e);
                process::exit(1);
            });
        }
        if !path.exists() {
            fs::write(&path, &contents).unwrap_or_else(|e| {
                eprintln!("Error writing {}: {}", path.display(), e);
                process::exit(1);
            });
            emitted_files.push((rel_path.clone(), contents.clone()));
        }
    }

    if let Some(smb) = source_map_builder {
        let map_path = output_file.with_extension("rs.cam.map");
        let json = smb.to_json();
        fs::write(&map_path, &json).unwrap_or_else(|e| {
            eprintln!("Error writing {}: {}", map_path.display(), e);
            process::exit(1);
        });
        if !quiet {
            println!("Source map: {}", map_path.display());
        }
    }

    for (rel_path, contents) in backend.gen_test_files(program, entity_name) {
        let path = output_dir.join(&rel_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap_or_else(|e| {
                eprintln!("Error creating {}: {}", parent.display(), e);
                process::exit(1);
            });
        }
        fs::write(&path, &contents).unwrap_or_else(|e| {
            eprintln!("Error writing {}: {}", path.display(), e);
            process::exit(1);
        });
        emitted_files.push((rel_path.clone(), contents));
        if !quiet {
            println!("  Tests: {}", path.display());
        }
    }

    if target == Target::Evm {
        cambrian_transpiler::manifest::write_evm_manifest(output_dir, &emitted_files)
            .unwrap_or_else(|e| {
                eprintln!(
                    "Error writing {}: {}",
                    cambrian_transpiler::manifest::MANIFEST_REL_PATH,
                    e
                );
                process::exit(1);
            });
    }

    if quiet {
        return;
    }

    println!(
        "Transpiled {} -> {} target",
        input_path,
        backend.target_description()
    );
    println!("  Output: {}", output_file.display());
    println!(
        "Entity: {}, {} routes, target: {}",
        entity_name,
        program
            .entities
            .first()
            .map(|e| e.routes.len())
            .unwrap_or(0),
        target.name(),
    );
    if !program.tests.is_empty() {
        println!("  {} test(s)", program.tests.len());
    }
    match target {
        Target::AckiNacki => {
            println!(
                "\nTo build WASM: cd {}/wasm && cargo component build --release --target wasm32-wasip2",
                output_dir.display()
            );
        }
        Target::Evm => {
            println!(
                "\nTo build: cd {} && bash setup.sh && forge test",
                output_dir.display()
            );
        }
        _ => {
            println!("\nTo build: cd {} && cargo build", output_dir.display());
        }
    }
}

fn snake_case(s: &str) -> String {
    let mut result = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                result.push('_');
            }
            result.extend(c.to_lowercase());
        } else {
            result.push(c);
        }
    }
    result
}
