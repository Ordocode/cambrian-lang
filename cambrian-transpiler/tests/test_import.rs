// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Phase Library-1: cross-file `import "..."` tests.
//!
//! Coverage:
//!   - Single-file roundtrip with one `import`.
//!   - Transitive imports (A → B → C) flatten correctly.
//!   - Cycle detection (A → B → A) does not loop.
//!   - Duplicate-name across files → merge error.
//!   - Imported file declaring `entity` → F4.
//!   - Single-file CLI walk picks up transitive imports automatically.

use std::fs;
use std::path::PathBuf;

use cambrian_transpiler::project;

/// Write `files` (relative paths + contents) under a fresh temp dir and
/// return the dir path. The dir is auto-deleted by the OS later; no
/// explicit cleanup is required for these unit-test fixtures.
fn write_fixture(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let stem = format!("cambrian_import_{}", name);
    let dir = std::env::temp_dir().join(stem);
    // Reset any leftover artefacts from a previous run.
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    for (rel, contents) in files {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, contents).unwrap();
    }
    dir
}

/// Minimal entity body — `set(v)` route with one member `m_v` that gets
/// updated in the route. Matches the canonical `contracts/counter.cam`
/// shape (members declared after `routes { ... }`, with `in <route>(...)`
/// transform syntax).
const FOO_USING_DOUBLE: &str = r#"
entity Foo {
    routes {
        set(v: u64) => []
    }

    m_v: u64 {
        in set(v) => double(v)
    }
}
"#;

const FOO_USING_DOUBLE_OF_TRIPLE: &str = r#"
entity Foo {
    routes {
        set(v: u64) => []
    }

    m_v: u64 {
        in set(v) => double(triple(v))
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

#[test]
fn parse_single_import_directive() {
    // Grammar smoke test — bare parse of an `import` directive.
    let source = r#"
        import "./helpers.cam"
    "#
    .to_string()
        + FOO_PASSTHROUGH;
    let program = cambrian_transpiler::ProgramParser::new()
        .parse(&source)
        .unwrap();
    assert_eq!(program.file_imports.len(), 1);
    assert_eq!(program.file_imports[0].path, "./helpers.cam");
}

#[test]
fn parse_bare_import_directive() {
    let source = r#"
        import "token/core.cam"
    "#
    .to_string()
        + FOO_PASSTHROUGH;
    let program = cambrian_transpiler::ProgramParser::new()
        .parse(&source)
        .unwrap();
    assert_eq!(program.file_imports.len(), 1);
    assert_eq!(program.file_imports[0].path, "token/core.cam");
}

#[test]
fn flat_import_pulls_in_pure_fn() {
    let dir = write_fixture(
        "flat_pure_fn",
        &[
            ("helpers.cam", "pure fn double(x: u64) -> u64 { x * 2 }\n"),
            (
                "entry.cam",
                &(r#"import "./helpers.cam""#.to_string() + FOO_USING_DOUBLE),
            ),
        ],
    );

    let entries = vec![dir.join("entry.cam")];
    let tagged = project::load_with_imports(&entries, &dir).unwrap();
    let merged = project::merge_tagged_for_single_file(&tagged).unwrap();

    assert_eq!(
        merged.pure_fns.len(),
        1,
        "expected `double` to be pulled in"
    );
    assert_eq!(merged.pure_fns[0].name, "double");
    assert_eq!(merged.entities.len(), 1);
    assert_eq!(merged.entities[0].name, "Foo");
}

#[test]
fn transitive_imports_a_b_c_flatten() {
    let dir = write_fixture(
        "transitive_a_b_c",
        &[
            (
                "c.cam",
                "pure fn triple(x: u64) -> u64 { x * 3 }\nrecord CResult { value: u64 }\n",
            ),
            (
                "b.cam",
                "import \"./c.cam\"\n\
             pure fn double(x: u64) -> u64 { x * 2 }\n\
             type Amount = u64\n",
            ),
            (
                "a.cam",
                &(r#"import "./b.cam""#.to_string() + FOO_USING_DOUBLE_OF_TRIPLE),
            ),
        ],
    );

    let entries = vec![dir.join("a.cam")];
    let tagged = project::load_with_imports(&entries, &dir).unwrap();
    let merged = project::merge_tagged_for_single_file(&tagged).unwrap();

    let fn_names: Vec<_> = merged.pure_fns.iter().map(|f| f.name.as_str()).collect();
    assert!(
        fn_names.contains(&"double"),
        "B's double must be present: {:?}",
        fn_names
    );
    assert!(
        fn_names.contains(&"triple"),
        "C's triple must be present: {:?}",
        fn_names
    );
    assert_eq!(merged.records.len(), 1);
    assert_eq!(merged.records[0].name, "CResult");
    let alias_names: Vec<_> = merged
        .type_aliases
        .iter()
        .map(|t| t.name.as_str())
        .collect();
    assert!(alias_names.contains(&"Amount"));
}

#[test]
fn cycle_detection_does_not_loop() {
    // A imports B, B imports A. The BFS dedup on canonicalised paths
    // must terminate.
    let dir = write_fixture(
        "cycle_a_b",
        &[
            (
                "a.cam",
                "import \"./b.cam\"\npure fn from_a(x: u64) -> u64 { x }\n",
            ),
            (
                "b.cam",
                "import \"./a.cam\"\npure fn from_b(x: u64) -> u64 { x + 1 }\n",
            ),
        ],
    );

    let entries = vec![dir.join("a.cam")];
    let tagged = project::load_with_imports(&entries, &dir).unwrap();
    // Both files should be loaded once.
    assert_eq!(tagged.len(), 2);
}

#[test]
fn duplicate_pure_fn_across_files_errors() {
    let dir = write_fixture(
        "dup_pure_fn",
        &[
            ("helpers.cam", "pure fn double(x: u64) -> u64 { x * 2 }\n"),
            (
                "entry.cam",
                &(r#"import "./helpers.cam"
pure fn double(x: u64) -> u64 { x + x }
"#
                .to_string()
                    + FOO_USING_DOUBLE),
            ),
        ],
    );

    let entries = vec![dir.join("entry.cam")];
    let tagged = project::load_with_imports(&entries, &dir).unwrap();
    let result = project::merge_tagged_for_single_file(&tagged);
    let err = result.expect_err("duplicate pure_fn must produce a merge error");
    let msg = format!("{}", err);
    assert!(
        msg.contains("double"),
        "error must mention 'double': {}",
        msg
    );
}

#[test]
fn f4_imported_file_declares_entity() {
    let dir = write_fixture(
        "f4_entity_in_lib",
        &[
            (
                "lib_with_entity.cam",
                "pure fn double(x: u64) -> u64 { x * 2 }\n\
             entity LibraryEntity {\n\
                 routes { noop() => [] }\n\
                 m_x: u64 { in noop() => 0 }\n\
             }\n",
            ),
            (
                "entry.cam",
                &(r#"import "./lib_with_entity.cam""#.to_string() + FOO_USING_DOUBLE),
            ),
        ],
    );

    let entries = vec![dir.join("entry.cam")];
    let tagged = project::load_with_imports(&entries, &dir).unwrap();
    let result = project::merge_tagged_for_single_file(&tagged);
    let err = result.expect_err("F4 must reject entity in imported file");
    let msg = format!("{}", err);
    assert!(msg.contains("F4"), "expected F4 marker, got: {}", msg);
    assert!(
        !msg.contains("Merge conflict"),
        "F4 must not look like a merge conflict: {}",
        msg
    );
    assert!(
        msg.contains("LibraryEntity"),
        "must name the offending entity: {}",
        msg
    );
}

#[test]
fn f4_imported_file_declares_test() {
    // Tests / fuzz / invariants in imported files must be rejected by
    // F4. We use the same canonical entity shape as the rest of this
    // module so the test body parses cleanly.
    let dir = write_fixture(
        "f4_test_in_lib",
        &[
            (
                "lib_with_test.cam",
                "pure fn double(x: u64) -> u64 { x * 2 }\n\
             test \"imported test\" for Foo with { m_v: 0 } {\n\
                 call set(1)\n\
                 expect state { m_v: 1 }\n\
             }\n",
            ),
            (
                "entry.cam",
                &(r#"import "./lib_with_test.cam""#.to_string() + FOO_PASSTHROUGH),
            ),
        ],
    );

    let entries = vec![dir.join("entry.cam")];
    let tagged = project::load_with_imports(&entries, &dir).unwrap();
    let result = project::merge_tagged_for_single_file(&tagged);
    let err = result.expect_err("F4 must reject test in imported file");
    let msg = format!("{}", err);
    assert!(msg.contains("F4"), "expected F4 marker, got: {}", msg);
    assert!(
        !msg.contains("Merge conflict"),
        "F4 must not look like a merge conflict: {}",
        msg
    );
    assert!(msg.contains("test"), "must mention test: {}", msg);
}

#[test]
fn duplicate_import_warning_w8() {
    // Same path listed twice in source.
    let source = r#"
        import "./helpers.cam"
        import "./helpers.cam"
    "#
    .to_string()
        + FOO_PASSTHROUGH;
    let program = cambrian_transpiler::ProgramParser::new()
        .parse(&source)
        .unwrap();
    let diags = cambrian_transpiler::validate::validate(&program);
    let w8 = diags.iter().find(|d| d.code == "W8");
    assert!(w8.is_some(), "expected W8 warning, got: {:?}", diags);
}

#[test]
fn project_yaml_with_transitive_imports_loads() {
    // End-to-end: project.yaml lists only the entry file. The library
    // (helpers.cam) is reached via `import "./helpers.cam"` from the
    // entry. Verifies the project loader's transitive-import walk.
    let dir = write_fixture(
        "project_yaml_transitive",
        &[
            ("helpers.cam", "pure fn double(x: u64) -> u64 { x * 2 }\n"),
            (
                "Foo.cam",
                &(r#"import "./helpers.cam""#.to_string() + FOO_USING_DOUBLE),
            ),
            (
                "project.yaml",
                "name: foo-demo\n\
             target: evm\n\
             sources:\n\
             \x20\x20- Foo.cam\n",
            ),
        ],
    );

    let project = cambrian_transpiler::project::Project::load(&dir.join("project.yaml")).unwrap();
    assert!(
        project.merged.pure_fns.iter().any(|f| f.name == "double"),
        "helpers.cam was not pulled in via import"
    );
    assert!(
        project.merged.entities.iter().any(|e| e.name == "Foo"),
        "Foo.cam was not loaded"
    );
    // The loader should have recorded both files in its program list.
    assert_eq!(
        project.programs.len(),
        2,
        "expected 2 source programs, got {:?}",
        project
            .programs
            .iter()
            .map(|(s, _)| s.as_str())
            .collect::<Vec<_>>()
    );
}

#[test]
fn imports_propagate_aliases_and_extern_entities() {
    let dir = write_fixture(
        "alias_and_extern",
        &[
            (
                "interfaces.cam",
                "type Amount = u64\n\
             extern entity Token {\n\
                 view route balance(who: address) -> u64;\n\
             }\n",
            ),
            (
                "entry.cam",
                "import \"./interfaces.cam\"\n\
             entity Foo {\n\
                 routes { set(v: Amount) => [] }\n\
                 m_v: Amount { in set(v) => v }\n\
             }\n",
            ),
        ],
    );

    let entries = vec![dir.join("entry.cam")];
    let tagged = project::load_with_imports(&entries, &dir).unwrap();
    let merged = project::merge_tagged_for_single_file(&tagged).unwrap();

    assert!(merged.type_aliases.iter().any(|ta| ta.name == "Amount"));
    assert!(merged.extern_entities.iter().any(|e| e.name == "Token"));
}

#[test]
fn bare_import_falls_back_to_library_paths() {
    let root = write_fixture(
        "bare_lib_fallback",
        &[
            (
                "proj/Foo.cam",
                &(r#"import "token/core.cam""#.to_string() + FOO_USING_DOUBLE),
            ),
            (
                "proj/project.yaml",
                "target: evm\n\
                 sources:\n\
                 \x20\x20- Foo.cam\n\
                 library_paths:\n\
                 \x20\x20- ../lib\n",
            ),
            (
                "lib/token/core.cam",
                "pure fn double(x: u64) -> u64 { x * 2 }\n",
            ),
        ],
    );

    let project = project::Project::load(&root.join("proj/project.yaml")).unwrap();
    assert!(
        project.merged.pure_fns.iter().any(|f| f.name == "double"),
        "bare import must resolve via library_paths"
    );
}

#[test]
fn relative_import_miss_does_not_search_library_paths() {
    let root = write_fixture(
        "relative_miss_no_lib",
        &[
            (
                "proj/Foo.cam",
                &(r#"import "./core.cam""#.to_string() + FOO_USING_DOUBLE),
            ),
            (
                "proj/project.yaml",
                "target: evm\n\
                 sources:\n\
                 \x20\x20- Foo.cam\n\
                 library_paths:\n\
                 \x20\x20- ../lib\n",
            ),
            ("lib/core.cam", "pure fn double(x: u64) -> u64 { x * 2 }\n"),
        ],
    );

    let err = project::Project::load(&root.join("proj/project.yaml"))
        .expect_err("./ miss must not fall back to library_paths");
    let msg = format!("{err}");
    assert!(!msg.contains("F5"), "relative miss stays IO, not F5: {msg}");
    assert!(
        msg.contains("core.cam") || msg.contains("IO"),
        "expected IO mentioning the missing relative path: {msg}"
    );
}

#[test]
fn bare_import_miss_is_f5() {
    let root = write_fixture(
        "bare_miss_f5",
        &[
            (
                "proj/Foo.cam",
                &(r#"import "token/core.cam""#.to_string() + FOO_PASSTHROUGH),
            ),
            (
                "proj/project.yaml",
                "target: evm\n\
                 sources:\n\
                 \x20\x20- Foo.cam\n\
                 library_paths:\n\
                 \x20\x20- ../lib\n",
            ),
        ],
    );
    fs::create_dir_all(root.join("lib")).unwrap();

    match project::Project::load(&root.join("proj/project.yaml")) {
        Err(project::ProjectError::Packaging { code, message, .. }) => {
            assert_eq!(code, "F5");
            assert!(message.contains("token/core.cam"), "{message}");
        }
        other => panic!("expected F5 Packaging, got {other:?}"),
    }
}
