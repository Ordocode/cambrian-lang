// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

use proptest::prelude::*;
use cambrian_transpiler::ProgramParser;
use cambrian_transpiler::pretty::pretty_print;

fn parser() -> ProgramParser {
    ProgramParser::new()
}

fn arb_ident() -> impl Strategy<Value = String> {
    prop::string::string_regex("[a-z][a-z0-9_]{0,6}")
        .unwrap()
        .prop_filter("not a keyword", |s| {
            !matches!(s.as_str(),
                "pure" | "fn" | "entity" | "record" | "enum" | "const" | "macro"
                | "routes" | "if" | "else" | "let" | "in" | "as" | "match" | "type"
                | "view" | "from" | "some" | "none" | "array" | "where" | "throw"
                | "return" | "true" | "false" | "for" | "init"
            )
        })
}

fn arb_type_name() -> impl Strategy<Value = String> {
    prop::string::string_regex("[A-Z][a-zA-Z0-9]{0,5}")
        .unwrap()
}

fn arb_simple_type() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("u8".to_string()),
        Just("u32".to_string()),
        Just("u64".to_string()),
        Just("bool".to_string()),
        Just("String".to_string()),
        Just("address".to_string()),
    ]
}

fn arb_valid_entity() -> impl Strategy<Value = String> {
    (arb_type_name(), arb_ident(), prop::collection::vec(
        (arb_ident().prop_map(|s| format!("m_{}", s)), arb_simple_type()),
        0..=2
    )).prop_map(|(entity_name, route_name, members)| {
        let members_str = members.iter()
            .map(|(mn, ty)| format!("    {}: {} {{ in {}() => 0 }}", mn, ty, route_name))
            .collect::<Vec<_>>()
            .join("\n");
        format!("entity {} {{\n    routes {{\n        {}() => []\n    }}\n{}\n}}", entity_name, route_name, members_str)
    })
}

fn arb_pure_fn() -> impl Strategy<Value = String> {
    (arb_ident(), arb_simple_type(), (0u64..100_000))
        .prop_map(|(name, ret_type, body)| {
            format!("pure fn {}() -> {} {{ {} }}", name, ret_type, body)
        })
}

fn arb_record() -> impl Strategy<Value = String> {
    (arb_type_name(), prop::collection::vec(
        (arb_ident(), arb_simple_type()),
        1..=4
    )).prop_map(|(name, fields)| {
        let fs = fields.iter()
            .map(|(n, t)| format!("{}: {}", n, t))
            .collect::<Vec<_>>()
            .join(", ");
        format!("record {} {{ {} }}", name, fs)
    })
}

fn arb_enum() -> impl Strategy<Value = String> {
    (arb_type_name(), prop::collection::vec(arb_type_name(), 2..=5))
        .prop_map(|(name, variants)| {
            format!("enum {} {{ {} }}", name, variants.join(", "))
        })
}

// ===================================================================
// Pretty-print round-trip: parse → pretty → parse → compare AST
// ===================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn fuzz_pretty_roundtrip_entity(entity in arb_valid_entity()) {
        if let Ok(prog1) = parser().parse(&entity) {
            let printed = pretty_print(&prog1);
            let prog2 = parser().parse(&printed);
            prop_assert!(prog2.is_ok(),
                "Re-parse failed.\nOriginal:\n{}\nPrinted:\n{}\nError: {:?}",
                entity, printed, prog2.err());
            prop_assert_eq!(prog1, prog2.unwrap(),
                "AST mismatch after round-trip.\nOriginal:\n{}\nPrinted:\n{}", entity, printed);
        }
    }

    #[test]
    fn fuzz_pretty_roundtrip_pure_fn(src in arb_pure_fn()) {
        if let Ok(prog1) = parser().parse(&src) {
            let printed = pretty_print(&prog1);
            let prog2 = parser().parse(&printed);
            prop_assert!(prog2.is_ok(),
                "Re-parse failed.\nOriginal:\n{}\nPrinted:\n{}\nError: {:?}",
                src, printed, prog2.err());
            prop_assert_eq!(prog1, prog2.unwrap());
        }
    }

    #[test]
    fn fuzz_pretty_roundtrip_record(src in arb_record()) {
        if let Ok(prog1) = parser().parse(&src) {
            let printed = pretty_print(&prog1);
            let prog2 = parser().parse(&printed);
            prop_assert!(prog2.is_ok(),
                "Re-parse failed.\nOriginal:\n{}\nPrinted:\n{}\nError: {:?}",
                src, printed, prog2.err());
            prop_assert_eq!(prog1, prog2.unwrap());
        }
    }

    #[test]
    fn fuzz_pretty_roundtrip_enum(src in arb_enum()) {
        if let Ok(prog1) = parser().parse(&src) {
            let printed = pretty_print(&prog1);
            let prog2 = parser().parse(&printed);
            prop_assert!(prog2.is_ok(),
                "Re-parse failed.\nOriginal:\n{}\nPrinted:\n{}\nError: {:?}",
                src, printed, prog2.err());
            prop_assert_eq!(prog1, prog2.unwrap());
        }
    }
}

// ===================================================================
// Idempotency: pretty(pretty(x)) == pretty(x)
// ===================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    #[test]
    fn fuzz_pretty_idempotent(entity in arb_valid_entity()) {
        if let Ok(prog1) = parser().parse(&entity) {
            let p1 = pretty_print(&prog1);
            if let Ok(prog2) = parser().parse(&p1) {
                let p2 = pretty_print(&prog2);
                prop_assert_eq!(&p1, &p2,
                    "Pretty-print not idempotent.\nFirst:\n{}\nSecond:\n{}", p1, p2);
            }
        }
    }
}
