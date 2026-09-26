// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

use cambrian_transpiler::parse_recover::recover_parse_diagnostics;

#[test]
fn recover_five_independent_top_level_errors() {
    let src = r#"
entity A
entity B
entity C
entity D
entity E
"#;
    let diags = recover_parse_diagnostics(src, "five.cam", 32);
    assert!(
        diags.len() >= 4,
        "expected at least 4 parse diagnostics, got {}: {:?}",
        diags.len(),
        diags.iter().map(|d| (&d.code, &d.message, &d.span)).collect::<Vec<_>>()
    );
    assert!(diags.iter().all(|d| d.code.starts_with("PARSE_")));
    assert!(diags.iter().all(|d| d.path.as_deref() == Some("five.cam")));
    assert!(diags.iter().any(|d| d.span.as_ref().map(|s| s.line >= 1).unwrap_or(false)));
}

#[test]
fn parse_stamps_file_id_on_span() {
    let src = "entity E { routes { go() => [] } m_x: u64 { in go() => 0 } }";
    cambrian_transpiler::ast::reset_file_table();
    let _g = cambrian_transpiler::ast::begin_parse_file("stamp.cam", src);
    let p = cambrian_transpiler::ProgramParser::new()
        .parse(src)
        .expect("valid program");
    assert_ne!(p.entities[0].span.file_id, 0);
    let (path, _) = cambrian_transpiler::ast::file_source(p.entities[0].span.file_id)
        .expect("file table entry");
    assert_eq!(path, "stamp.cam");
}

#[test]
fn recover_valid_program_is_empty() {
    let src = r#"
entity E {
    routes { go() => [] }
    m_x: u64 { in go() => 0 }
}
"#;
    let diags = recover_parse_diagnostics(src, "ok.cam", 32);
    assert!(diags.is_empty(), "valid program must not emit recovery diags: {:?}", diags);
}

#[test]
fn recover_respects_max_errors() {
    let src = r#"
entity A
entity B
entity C
entity D
entity E
"#;
    let diags = recover_parse_diagnostics(src, "five.cam", 2);
    assert_eq!(diags.len(), 2);
}
