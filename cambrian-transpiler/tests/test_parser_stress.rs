// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

use cambrian_transpiler::ProgramParser;

fn parser() -> ProgramParser {
    ProgramParser::new()
}

#[test]
fn stress_large_entity_many_routes() {
    let routes: Vec<String> = (0..50)
        .map(|i| format!("        route_{}(x: u64) => []", i))
        .collect();
    let members: Vec<String> = (0..20)
        .map(|i| format!("    m_{}: u64 {{ in route_0(x) => x + {} }}", i, i))
        .collect();
    let src = format!(
        "entity Large {{\n    routes {{\n{}\n    }}\n{}\n}}",
        routes.join("\n"),
        members.join("\n")
    );
    let p = parser().parse(&src).expect("Large entity should parse");
    assert_eq!(p.entities[0].routes.len(), 50);
    assert_eq!(p.entities[0].members.len(), 20);
}

#[test]
fn stress_many_entities() {
    let entities: Vec<String> = (0..20)
        .map(|i| format!(
            "entity Entity{} {{\n    routes {{ setup() => [] }}\n    m_val: u64 {{ in setup() => {} }}\n}}",
            i, i
        ))
        .collect();
    let src = entities.join("\n\n");
    let p = parser().parse(&src).expect("Many entities should parse");
    assert_eq!(p.entities.len(), 20);
}

#[test]
fn stress_complex_pure_fn_let_chain() {
    let mut lets = String::new();
    for i in 0..30 {
        if i == 0 {
            lets.push_str(&format!("let v{} = x + {};\n", i, i));
        } else {
            lets.push_str(&format!("let v{} = v{} + {};\n", i, i - 1, i));
        }
    }
    let src = format!("pure fn complex(x: u64) -> u64 {{ {} v29 }}", lets);
    let p = parser().parse(&src).expect("Complex let chain should parse");
    assert_eq!(p.pure_fns.len(), 1);
}

#[test]
fn stress_nested_block_depth() {
    let depth = 10;
    let mut inner = "x + 1".to_string();
    for _ in 0..depth {
        inner = format!("{{ let tmp = {}; tmp }}", inner);
    }
    let src = format!("pure fn deep_blocks(x: u64) -> u64 {{ let result = {}; result }}", inner);
    let p = parser().parse(&src).expect("Deeply nested blocks should parse");
    assert_eq!(p.pure_fns.len(), 1);
}

#[test]
fn stress_nested_if_depth() {
    let mut body = "0".to_string();
    for i in (0..15).rev() {
        body = format!("if x > {} {{ {} }} else {{ {} }}", i, body, i);
    }
    let src = format!("pure fn deep_if(x: u64) -> u64 {{ {} }}", body);
    let p = parser().parse(&src).expect("Deep nested if should parse");
    assert_eq!(p.pure_fns.len(), 1);
}

#[test]
fn stress_large_record() {
    let fields: Vec<String> = (0..50)
        .map(|i| format!("field_{}: u64", i))
        .collect();
    let src = format!("record HugeRecord {{ {} }}", fields.join(", "));
    let p = parser().parse(&src).expect("Large record should parse");
    assert_eq!(p.records[0].fields.len(), 50);
}

#[test]
fn stress_large_enum() {
    let variants: Vec<String> = (0..50)
        .map(|i| format!("Variant{}", i))
        .collect();
    let src = format!("enum HugeEnum {{ {} }}", variants.join(", "));
    let p = parser().parse(&src).expect("Large enum should parse");
    assert_eq!(p.enums[0].variants.len(), 50);
}

#[test]
fn stress_many_type_aliases() {
    let aliases: Vec<String> = (0..50)
        .map(|i| format!("type Alias{} = u64", i))
        .collect();
    let src = aliases.join("\n");
    let p = parser().parse(&src).expect("Many type aliases should parse");
    assert_eq!(p.type_aliases.len(), 50);
}

#[test]
fn stress_many_pure_fns() {
    let fns: Vec<String> = (0..50)
        .map(|i| format!("pure fn fn_{}(x: u64) -> u64 {{ x + {} }}", i, i))
        .collect();
    let src = fns.join("\n");
    let p = parser().parse(&src).expect("Many pure fns should parse");
    assert_eq!(p.pure_fns.len(), 50);
}

#[test]
fn stress_member_many_transforms() {
    let routes: Vec<String> = (0..30)
        .map(|i| format!("        route_{}(x: u64) => []", i))
        .collect();
    let transforms: Vec<String> = (0..30)
        .map(|i| format!("        in route_{}(x) => m_val + x + {}", i, i))
        .collect();
    let src = format!(
        "entity E {{\n    routes {{\n{}\n    }}\n    m_val: u64 {{\n{}\n    }}\n}}",
        routes.join("\n"),
        transforms.join("\n")
    );
    let p = parser().parse(&src).expect("Many transforms should parse");
    assert_eq!(p.entities[0].members[0].transforms.len(), 30);
}

#[test]
fn stress_route_many_where_clauses() {
    let wheres: Vec<String> = (0..15)
        .map(|i| format!("&& x > {} : throw {}", i, 100 + i))
        .collect();
    let src = format!(
        "entity E {{ routes {{ go(x: u64) where x >= 0 : throw 99 {} => [] }} }}",
        wheres.join(" ")
    );
    let p = parser().parse(&src).expect("Many where clauses should parse");
    assert_eq!(p.entities[0].routes[0].where_clauses.len(), 16);
}

#[test]
fn stress_route_many_actions() {
    let actions: Vec<String> = (0..30)
        .map(|i| format!("        msg_{}({}) ~> target", i, i))
        .collect();
    let src = format!(
        "entity E {{ routes {{ go() => [\n{}\n    ] }} }}",
        actions.join("\n")
    );
    let p = parser().parse(&src).expect("Many actions should parse");
    assert_eq!(p.entities[0].routes[0].body.actions().len(), 30);
}

#[test]
fn stress_combinatorial_features() {
    let src = r#"
        type Bal = u128

        enum Status { On, Off }

        record Rec { x: u64, y: String }

        pure fn calc(a: Bal, b: Bal) -> Bal {
            if a > b { a - b } else { b - a }
        }

        entity Multi {
            type Inner = u64

            enum Mode { Fast, Slow, Normal }

            record Config { timeout: Inner, retries: u8 }

            const LIMIT: Inner = 100

            macro is_on() -> bool = {
                m_status == Status::On
            }

            routes {
                setup(s: Status) => []

                update(val: Bal)
                from Admin(m_admin)
                where val > 0 : throw 1
                => [
                    Updated(val) ~> msg::sender
                ]

                view info() -> Bal => [
                    return(m_balance)
                ]

                pure double(x: Inner) -> Inner => [
                    return(x * 2)
                ]
            }

            m_status: Status {
                in setup(s) => s
            }

            m_balance: Bal {
                in setup(_) => 0
                in update(val) => m_balance + val
            }

            m_admin: address {
                in setup(_) => msg::sender
            }

            m_tx_count: Inner {
                in update(_) => ^m_tx_count + 1
            }
        }
    "#;

    let p = parser().parse(src).expect("Combinatorial program should parse");
    assert_eq!(p.entities.len(), 1);
    let e = &p.entities[0];

    assert_eq!(e.type_aliases.len(), 1);
    assert_eq!(e.enums.len(), 1);
    assert_eq!(e.records.len(), 1);
    assert_eq!(e.constants.len(), 1);
    assert_eq!(e.macros.len(), 1);
    assert_eq!(e.routes.len(), 4);
    assert_eq!(e.members.len(), 4);

    assert!(!e.routes[0].is_view && !e.routes[0].is_pure);
    assert!(!e.routes[1].is_view && !e.routes[1].is_pure);
    assert!(e.routes[2].is_view);
    assert!(e.routes[3].is_pure);
    assert_eq!(e.routes[1].from_clauses.len(), 1);
    assert_eq!(e.routes[1].where_clauses.len(), 1);
}
