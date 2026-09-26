// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

use cambrian_transpiler::ProgramParser;
use cambrian_transpiler::ast::*;
use cambrian_transpiler::codegen::{
    CodegenCtx, cambrian_function_id, default_ser_be, compute_default_state_hex,
    is_complex_type, is_complex_rust_type, gen_type,
};
use std::collections::HashMap;

fn parse(src: &str) -> Program {
    ProgramParser::new().parse(src).unwrap_or_else(|e| panic!("Parse error: {e}"))
}

// ---------------------------------------------------------------------------
// Part 1: CodegenCtx tests
// ---------------------------------------------------------------------------

#[test]
fn ctx_new_populates_entity_routes() {
    let prog = parse(r#"
        entity Foo {
            routes { inc(n: u32) => [] }
            m_x: u64 { in inc(n) => m_x + n }
        }
    "#);
    let ctx = CodegenCtx::new(&prog);
    let foo_routes = ctx.entity_routes.get("Foo").expect("missing Foo");
    let inc_params = foo_routes.get("inc").expect("missing inc");
    assert_eq!(inc_params, &vec![Type::Simple("u32".to_string())]);
}

#[test]
fn ctx_new_constructor_is_mapped() {
    let prog = parse(r#"
        entity Bar {
            routes { constructor(v: u64) => [] }
            m_x: u64 { in constructor(v) => v }
        }
    "#);
    let ctx = CodegenCtx::new(&prog);
    let bar_routes = ctx.entity_routes.get("Bar").unwrap();
    assert!(bar_routes.contains_key("constructor"));
}

#[test]
fn ctx_resolve_type_simple_passthrough() {
    let prog = parse("entity X { }");
    let ctx = CodegenCtx::new(&prog);
    let ty = Type::Simple("u64".to_string());
    assert_eq!(ctx.resolve_type(&ty), Type::Simple("u64".to_string()));
}

#[test]
fn ctx_resolve_type_alias() {
    let prog = parse("entity X { }");
    let mut aliases = HashMap::new();
    aliases.insert("Amount".to_string(), Type::Simple("u128".to_string()));
    let ctx = CodegenCtx::with_aliases(&prog, aliases);
    assert_eq!(
        ctx.resolve_type(&Type::Simple("Amount".to_string())),
        Type::Simple("u128".to_string()),
    );
}

#[test]
fn ctx_resolve_type_nested_alias() {
    let prog = parse("entity X { }");
    let mut aliases = HashMap::new();
    aliases.insert("A".to_string(), Type::Simple("B".to_string()));
    aliases.insert("B".to_string(), Type::Simple("u64".to_string()));
    let ctx = CodegenCtx::with_aliases(&prog, aliases);
    assert_eq!(
        ctx.resolve_type(&Type::Simple("A".to_string())),
        Type::Simple("u64".to_string()),
    );
}

#[test]
fn ctx_resolve_type_generic_with_alias() {
    let prog = parse("entity X { }");
    let mut aliases = HashMap::new();
    aliases.insert("Token".to_string(), Type::Simple("u128".to_string()));
    let ctx = CodegenCtx::with_aliases(&prog, aliases);
    let ty = Type::Generic("Vec".to_string(), vec![Type::Simple("Token".to_string())]);
    assert_eq!(
        ctx.resolve_type(&ty),
        Type::Generic("Vec".to_string(), vec![Type::Simple("u128".to_string())]),
    );
}

#[test]
fn ctx_resolve_type_tuple() {
    let prog = parse("entity Z { }");
    let mut aliases = HashMap::new();
    aliases.insert("X".to_string(), Type::Simple("u32".to_string()));
    let ctx = CodegenCtx::with_aliases(&prog, aliases);
    let ty = Type::Tuple(vec![
        Type::Simple("X".to_string()),
        Type::Simple("bool".to_string()),
    ]);
    assert_eq!(
        ctx.resolve_type(&ty),
        Type::Tuple(vec![
            Type::Simple("u32".to_string()),
            Type::Simple("bool".to_string()),
        ]),
    );
}

#[test]
fn ctx_resolve_type_typed_address_unchanged() {
    let prog = parse("entity X { }");
    let ctx = CodegenCtx::new(&prog);
    let ty = Type::TypedAddress("Foo".to_string());
    assert_eq!(ctx.resolve_type(&ty), Type::TypedAddress("Foo".to_string()));
}

#[test]
fn ctx_lookup_route_types_found() {
    let prog = parse(r#"
        entity A {
            routes { ping(n: u32) => [] }
            m_x: u64 { in ping(n) => m_x + n }
        }
        entity B {
            routes { pong(s: String) => [] }
            m_y: u64 { in pong(_) => 0 }
        }
    "#);
    let ctx = CodegenCtx::new(&prog);
    assert_eq!(
        ctx.lookup_route_types("A", "ping"),
        Some(vec![Type::Simple("u32".to_string())]),
    );
    assert_eq!(
        ctx.lookup_route_types("B", "pong"),
        Some(vec![Type::Simple("String".to_string())]),
    );
    assert_eq!(ctx.lookup_route_types("A", "pong"), None);
}

#[test]
fn ctx_lookup_entity_for_route_unique() {
    let prog = parse(r#"
        entity A {
            routes { ping(n: u32) => [] }
            m_x: u64 { in ping(n) => m_x + n }
        }
        entity B {
            routes { pong(s: String) => [] }
            m_y: u64 { in pong(_) => 0 }
        }
    "#);
    let ctx = CodegenCtx::new(&prog);
    assert_eq!(ctx.lookup_entity_for_route("ping", "B"), Some("A".to_string()));
    assert_eq!(ctx.lookup_entity_for_route("pong", "A"), Some("B".to_string()));
}

#[test]
fn ctx_lookup_entity_for_route_ambiguous() {
    let prog = parse(r#"
        entity A {
            routes { shared() => [] }
            m_x: u64 { in shared() => 0 }
        }
        entity B {
            routes { shared() => [] }
            m_y: u64 { in shared() => 0 }
        }
    "#);
    let ctx = CodegenCtx::new(&prog);
    assert_eq!(ctx.lookup_entity_for_route("shared", "C"), None);
}

#[test]
fn ctx_lookup_entity_for_route_missing() {
    let prog = parse(r#"
        entity A {
            routes { shared() => [] }
            m_x: u64 { in shared() => 0 }
        }
        entity B {
            routes { shared() => [] }
            m_y: u64 { in shared() => 0 }
        }
    "#);
    let ctx = CodegenCtx::new(&prog);
    assert_eq!(ctx.lookup_entity_for_route("nonexistent", "A"), None);
}

#[test]
fn ctx_lookup_entity_for_route_excludes_self() {
    let prog = parse(r#"
        entity A {
            routes { shared() => [] }
            m_x: u64 { in shared() => 0 }
        }
        entity B {
            routes { shared() => [] }
            m_y: u64 { in shared() => 0 }
        }
    "#);
    let ctx = CodegenCtx::new(&prog);
    assert_eq!(
        ctx.lookup_entity_for_route("shared", "A"),
        Some("B".to_string()),
    );
}

// ---------------------------------------------------------------------------
// Part 2: codegen/types.rs helpers
// ---------------------------------------------------------------------------

#[test]
fn function_id_deterministic() {
    assert_eq!(
        cambrian_function_id("increment"),
        cambrian_function_id("increment"),
    );
}

#[test]
fn function_id_different_names_differ() {
    assert_ne!(
        cambrian_function_id("increment"),
        cambrian_function_id("decrement"),
    );
}

#[test]
fn function_id_known_value() {
    assert_eq!(cambrian_function_id(""), 0x811c9dc5);
}

#[test]
fn is_complex_type_simple_types() {
    assert!(!is_complex_type(&Type::Simple("u64".to_string())));
    assert!(is_complex_type(&Type::Simple("String".to_string())));
}

#[test]
fn is_complex_type_generic() {
    assert!(is_complex_type(&Type::Generic(
        "Vec".to_string(),
        vec![Type::Simple("u8".to_string())],
    )));
    assert!(!is_complex_type(&Type::Generic(
        "SomeCustom".to_string(),
        vec![],
    )));
}

#[test]
fn is_complex_type_tuple() {
    assert!(is_complex_type(&Type::Tuple(vec![
        Type::Simple("u64".to_string()),
        Type::Simple("bool".to_string()),
    ])));
}

#[test]
fn is_complex_rust_type_checks() {
    assert!(!is_complex_rust_type("u64"));
    assert!(is_complex_rust_type("String"));
    assert!(is_complex_rust_type("HashMap<u32, u64>"));
    assert!(is_complex_rust_type("Vec<u8>"));
    assert!(is_complex_rust_type("(u32, u64)"));
}

#[test]
fn default_ser_be_u8() {
    assert_eq!(
        default_ser_be(&Type::Simple("u8".to_string()), &|_| None, &HashMap::new()),
        vec![0],
    );
}

#[test]
fn default_ser_be_u64() {
    assert_eq!(
        default_ser_be(&Type::Simple("u64".to_string()), &|_| None, &HashMap::new()),
        vec![0, 0, 0, 0, 0, 0, 0, 0],
    );
}

#[test]
fn default_ser_be_bool() {
    let result = default_ser_be(&Type::Simple("bool".to_string()), &|_| None, &HashMap::new());
    assert_eq!(result.len(), 1);
}

#[test]
fn default_ser_be_string() {
    let result = default_ser_be(&Type::Simple("String".to_string()), &|_| None, &HashMap::new());
    assert_eq!(result, vec![0, 0, 0, 0]);
}

#[test]
fn default_ser_be_with_alias() {
    let mut alias_map = HashMap::new();
    alias_map.insert("Amount".to_string(), Type::Simple("u64".to_string()));
    assert_eq!(
        default_ser_be(&Type::Simple("Amount".to_string()), &|_| None, &alias_map),
        default_ser_be(&Type::Simple("u64".to_string()), &|_| None, &HashMap::new()),
    );
}

#[test]
fn compute_default_state_hex_simple() {
    let entity = Entity {
        name: "Test".to_string(),
        members: vec![Member {
            name: "m_x".to_string(),
            ty: Type::Simple("u64".to_string()),
            transforms: vec![],
            default_value: None,
            is_identity: false,
            span: Span::none(),
        }],
        routes: vec![],
        macros: vec![],
        constants: vec![],
        type_aliases: vec![],
        records: vec![],
        enums: vec![],
        events: vec![],
        errors: vec![],
        span: Span::none(),
    };
    let hex = compute_default_state_hex(&entity, &|_| None, &HashMap::new());
    assert_eq!(hex, "0000000000000000");
}

#[test]
fn gen_type_simple() {
    assert_eq!(gen_type(&Type::Simple("u64".to_string())), "u64");
}

#[test]
fn gen_type_generic() {
    assert_eq!(
        gen_type(&Type::Generic(
            "HashMap".to_string(),
            vec![
                Type::Simple("String".to_string()),
                Type::Simple("u64".to_string()),
            ],
        )),
        "HashMap<String, u64>",
    );
}

#[test]
fn gen_type_tuple() {
    assert_eq!(
        gen_type(&Type::Tuple(vec![
            Type::Simple("u32".to_string()),
            Type::Simple("bool".to_string()),
        ])),
        "(u32, bool)",
    );
}

#[test]
fn gen_type_single_tuple() {
    assert_eq!(
        gen_type(&Type::Tuple(vec![Type::Simple("u32".to_string())])),
        "(u32,)",
    );
}
