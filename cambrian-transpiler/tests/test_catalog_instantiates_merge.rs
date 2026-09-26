// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! CAM-H-02 — catalog merge / desugar integration tests.

use cambrian_transpiler::ast::{
    InvariantEmitPolicy, PropertyInstanceKind, TestStep,
};
use cambrian_transpiler::desugar::desugar_properties;
use cambrian_transpiler::merge_catalog::merge_catalog_instantiations;

const ENTITY: &str = r"
entity E {
    routes {
        go() => []
        view totalSupply() -> u64 => [ return(m_x) ]
    }
    m_x: u64 { in go() => 0 }
}
";

fn parse(src: &str) -> cambrian_transpiler::ast::Program {
    let mut program = cambrian_transpiler::ProgramParser::new()
        .parse(src)
        .unwrap_or_else(|e| panic!("Parse error: {e}"));
    cambrian_transpiler::ast::normalize_program_types(&mut program);
    program
}

#[test]
fn merge_test_link_attaches_overlay_instance() {
    let src = format!(
        r#"{ENTITY}
property "PROP-1 base" () for E {{
    call go()
    expect state {{ m_x: 0 }}
}}
#[instantiates("PROP-1")]
test "linked scenario" for E {{
    call go()
    expect state {{ m_x: 1 }}
}}
"#
    );
    let mut program = parse(&src);
    assert_eq!(program.tests.len(), 1);
    merge_catalog_instantiations(&mut program);
    assert!(program.tests.is_empty());
    assert_eq!(program.properties.len(), 1);
    assert_eq!(program.properties[0].instances.len(), 1);
    let inst = &program.properties[0].instances[0];
    assert_eq!(inst.kind, PropertyInstanceKind::Test);
    assert!(!inst.replace_catalog_body);
    assert_eq!(inst.name.as_deref(), Some("linked scenario"));
}

#[test]
fn merge_property_link_extends_catalog_fuzz() {
    let src = format!(
        r#"{ENTITY}
property "PROP-1 base" (n: u64) for E {{
    assume n > 0
    call go()
}}
#[instantiates("PROP-1")]
property "CP-1 narrow" (n: u64) for E {{
    assume n <= 10
}}
"#
    );
    let mut program = parse(&src);
    merge_catalog_instantiations(&mut program);
    assert_eq!(program.properties.len(), 1);
    let inst = &program.properties[0].instances[0];
    assert_eq!(inst.kind, PropertyInstanceKind::Fuzz);
    assert!(!inst.replace_catalog_body);
    assert_eq!(inst.body_delta.len(), 1);
    assert!(matches!(inst.body_delta[0], TestStep::Assume { .. }));
}

#[test]
fn desugar_linked_integration_uses_catalog_body() {
    let src = format!(
        r#"{ENTITY}
property "PROP-1 base" () for E {{
    call go()
    expect state {{ m_x: 0 }}
}}
#[instantiates("PROP-1")]
test "integration path" for E {{
    msg {{ sender: 0x00000000000000000000000000000000000000d1 }}
}}
"#
    );
    let mut program = parse(&src);
    merge_catalog_instantiations(&mut program);
    desugar_properties(&mut program);
    assert_eq!(program.tests.len(), 1);
    let test = &program.tests[0];
    assert!(test.name.contains("integration path"));
    assert!(
        matches!(&test.body[0], TestStep::SetContext { .. }),
        "harness pins precede catalog steps"
    );
    assert!(test.body.iter().any(|s| matches!(s, TestStep::Call { .. })));
    assert!(test.body.iter().any(|s| matches!(s, TestStep::ExpectState { .. })));
}

#[test]
fn merge_invariant_link_overlays_catalog_and_supersedes_root() {
    let catalog_src = format!(
        r#"{ENTITY}
invariant "INV-1 base" for E {{
    action go() {{ }}
    check m_x == 0
}}
"#
    );
    let overlay_src = r#"
invariant "CP overlay" #[instantiates("INV-1")] for E {
    init { m_x: 1 }
}
"#;
    let mut program = parse(&catalog_src);
    let overlay = parse(overlay_src);
    program.invariants.extend(overlay.invariants);
    merge_catalog_instantiations(&mut program);
    assert_eq!(program.invariants.len(), 2);
    let catalog = program
        .invariants
        .iter()
        .find(|i| i.name.starts_with("INV-1"))
        .expect("catalog invariant retained");
    assert_eq!(catalog.emit_policy, InvariantEmitPolicy::Superseded);
    let merged = program
        .invariants
        .iter()
        .find(|i| i.name == "CP overlay")
        .expect("merged supplemental invariant");
    assert_eq!(merged.instances[0].init.len(), 1);
    assert_eq!(merged.actions.len(), 1);
    assert_eq!(merged.checks.len(), 1);
    assert!(merged.instantiates.is_none());
}

#[test]
fn merge_invariant_empty_init_clears_catalog_pins() {
    let catalog_src = format!(
        r#"{ENTITY}
invariant "INV-1 base" for E {{
    init {{ m_x: 9 }}
    action go() {{ }}
    check m_x == 0
}}
"#
    );
    let overlay_src = r#"
invariant "CP clear init" #[instantiates("INV-1")] for E {
    init { }
}
"#;
    let mut program = parse(&catalog_src);
    let overlay = parse(overlay_src);
    program.invariants.extend(overlay.invariants);
    merge_catalog_instantiations(&mut program);
    let merged = program
        .invariants
        .iter()
        .find(|i| i.name == "CP clear init")
        .expect("merged supplemental invariant");
    assert!(merged.instances[0].init.is_empty());
}

fn merge_invariant_deploy_overlay_replaces_catalog() {
    let catalog_src = format!(
        r#"{ENTITY}
invariant "INV-1 base" for E {{
    deploy {{ 0x0000000000000000000000000000000000000000000000000000000000000d00 }}
    action go() {{ }}
    check m_x == 0
}}
"#
    );
    let overlay_src = r#"
invariant "CP deploy overlay" #[instantiates("INV-1")] for E {
    deploy { 0x0000000000000000000000000000000000000000000000000000000000000d01 }
}
"#;
    let mut program = parse(&catalog_src);
    let overlay = parse(overlay_src);
    program.invariants.extend(overlay.invariants);
    merge_catalog_instantiations(&mut program);
    let merged = program
        .invariants
        .iter()
        .find(|i| i.name == "CP deploy overlay")
        .expect("merged supplemental invariant");
    assert_eq!(merged.deploy.len(), 1);
}
