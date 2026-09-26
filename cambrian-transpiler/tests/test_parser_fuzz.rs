// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

use proptest::prelude::*;
use cambrian_transpiler::ProgramParser;

#[allow(dead_code)]
fn parser() -> ProgramParser {
    ProgramParser::new()
}

#[allow(dead_code)]
fn parses_ok(src: &str) -> bool {
    parser().parse(src).is_ok()
}

#[allow(dead_code)]
fn parses_err(src: &str) -> bool {
    parser().parse(src).is_err()
}

// ===================================================================
// Property-based: random valid Cambrian programs must parse
// ===================================================================

fn arb_ident() -> impl Strategy<Value = String> {
    prop::string::string_regex("[a-z][a-z0-9_]{0,8}")
        .unwrap()
        .prop_filter("not a keyword", |s| {
            !matches!(s.as_str(),
                "pure" | "fn" | "entity" | "record" | "enum" | "const" | "macro"
                | "routes" | "if" | "else" | "let" | "in" | "as" | "match" | "type"
                | "view" | "from" | "some" | "none" | "array" | "where" | "throw"
                | "return" | "true" | "false" | "for" | "init" | "var"
                // Reserved tokens that cannot be route/member names. (`check` /
                // `action` / `senders` are dual-homed in IdentStr; `ctx` is not
                // — optional `ctx { }` conflicts with IdentStr if dual-homed.)
                | "accept" | "onbounce" | "identity" | "deploy" | "rescue"
                | "recover" | "private" | "call" | "test" | "expect" | "effects"
                | "skip" | "registry" | "fuzz" | "property" | "assume" | "bound"
                | "invariant" | "action" | "check" | "senders" | "ctx"
                | "track" | "derived" | "exclude" | "selectors" | "advancetime"
                | "extern" | "route" | "event" | "indexed" | "emit" | "error"
                | "use" | "import" | "library" | "using" | "with"
            )
        })
}

fn arb_type_name() -> impl Strategy<Value = String> {
    prop::string::string_regex("[A-Z][a-zA-Z0-9]{0,6}")
        .unwrap()
}

fn arb_simple_type() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("u8".to_string()),
        Just("u16".to_string()),
        Just("u32".to_string()),
        Just("u64".to_string()),
        Just("u128".to_string()),
        Just("bool".to_string()),
        Just("String".to_string()),
        Just("address".to_string()),
    ]
}

fn arb_literal() -> impl Strategy<Value = String> {
    prop_oneof![
        (0u64..1_000_000).prop_map(|n| n.to_string()),
        prop::bool::ANY.prop_map(|b| b.to_string()),
        Just("\"hello\"".to_string()),
    ]
}

#[allow(dead_code)]
fn arb_expr() -> impl Strategy<Value = String> {
    prop_oneof![
        arb_literal(),
        arb_ident(),
        (arb_literal(), arb_literal()).prop_map(|(a, b)| format!("{} + {}", a, b)),
        (arb_literal(), arb_literal()).prop_map(|(a, b)| format!("{} == {}", a, b)),
    ]
}

fn arb_member_name() -> impl Strategy<Value = String> {
    arb_ident().prop_map(|s| format!("m_{}", s))
}

fn arb_param() -> impl Strategy<Value = String> {
    (arb_ident(), arb_simple_type()).prop_map(|(n, t)| format!("{}: {}", n, t))
}

fn arb_params(max: usize) -> impl Strategy<Value = String> {
    prop::collection::vec(arb_param(), 0..=max)
        .prop_map(|ps| ps.join(", "))
}

fn arb_route() -> impl Strategy<Value = String> {
    (arb_ident(), arb_params(3)).prop_map(|(name, params)| {
        format!("{}({}) => []", name, params)
    })
}

#[allow(dead_code)]
fn arb_member(route_names: Vec<String>) -> impl Strategy<Value = String> {
    (arb_member_name(), arb_simple_type(), prop::sample::select(route_names))
        .prop_map(|(name, ty, route)| {
            format!("{}: {} {{ in {}() => 0 }}", name, ty, route)
        })
}

fn arb_entity() -> impl Strategy<Value = String> {
    (
        arb_type_name(),
        prop::collection::vec(arb_route(), 1..=4),
        prop::collection::vec(arb_member_name(), 0..=3),
    ).prop_flat_map(|(entity_name, routes, member_names)| {
        let route_strs: Vec<String> = routes.clone();
        let route_names: Vec<String> = routes.iter()
            .map(|r| r.split('(').next().unwrap().to_string())
            .collect();

        let members = if route_names.is_empty() || member_names.is_empty() {
            Just(vec![]).boxed()
        } else {
            let rn = route_names.clone();
            prop::collection::vec(
                (prop::sample::select(member_names), arb_simple_type(), prop::sample::select(rn))
                    .prop_map(|(mn, ty, rn)| format!("    {}: {} {{ in {}() => 0 }}", mn, ty, rn)),
                0..=3
            ).boxed()
        };

        (Just(entity_name), Just(route_strs), members).prop_map(|(name, routes, members)| {
            let routes_block = routes.iter()
                .map(|r| format!("        {}", r))
                .collect::<Vec<_>>()
                .join("\n");
            let members_block = members.join("\n");
            format!("entity {} {{\n    routes {{\n{}\n    }}\n{}\n}}", name, routes_block, members_block)
        })
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn fuzz_random_entity_parses(entity in arb_entity()) {
        let result = parser().parse(&entity);
        prop_assert!(result.is_ok(), "Failed to parse:\n{}\nError: {:?}", entity, result.err());
    }

    #[test]
    fn fuzz_pure_fn_parses(
        name in arb_ident(),
        params in arb_params(3),
        ret_type in arb_simple_type(),
        body in arb_literal(),
    ) {
        let src = format!("pure fn {}({}) -> {} {{ {} }}", name, params, ret_type, body);
        let result = parser().parse(&src);
        prop_assert!(result.is_ok(), "Failed to parse:\n{}\nError: {:?}", src, result.err());
    }

    #[test]
    fn fuzz_type_alias_parses(
        name in arb_type_name(),
        ty in arb_simple_type(),
    ) {
        let src = format!("type {} = {}", name, ty);
        let result = parser().parse(&src);
        prop_assert!(result.is_ok(), "Failed to parse:\n{}\nError: {:?}", src, result.err());
    }

    #[test]
    fn fuzz_record_parses(
        name in arb_type_name(),
        fields in prop::collection::vec(
            (arb_ident(), arb_simple_type()).prop_map(|(n, t)| format!("{}: {}", n, t)),
            1..=5
        ),
    ) {
        let src = format!("record {} {{ {} }}", name, fields.join(", "));
        let result = parser().parse(&src);
        prop_assert!(result.is_ok(), "Failed to parse:\n{}\nError: {:?}", src, result.err());
    }

    #[test]
    fn fuzz_enum_parses(
        name in arb_type_name(),
        variants in prop::collection::vec(arb_type_name(), 1..=6),
    ) {
        let src = format!("enum {} {{ {} }}", name, variants.join(", "));
        let result = parser().parse(&src);
        prop_assert!(result.is_ok(), "Failed to parse:\n{}\nError: {:?}", src, result.err());
    }

    #[test]
    fn fuzz_random_bytes_dont_crash(data in prop::collection::vec(any::<u8>(), 0..200)) {
        let src = String::from_utf8_lossy(&data);
        let _ = parser().parse(&src);
        // No panic = success
    }

    #[test]
    fn fuzz_ascii_strings_dont_crash(data in "[a-zA-Z0-9_ {}()\\[\\]:,=>~^@!+\\-*/<>&|;.\n]{0,300}") {
        let _ = parser().parse(&data);
    }
}

// ===================================================================
// Property-based: expression parsing
// ===================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn fuzz_int_literals(n in 0u64..u64::MAX) {
        let src = format!("pure fn f() -> u64 {{ {} }}", n);
        let result = parser().parse(&src);
        prop_assert!(result.is_ok(), "Failed for {}: {:?}", n, result.err());
    }

    #[test]
    fn fuzz_hex_literals(n in 0u64..0xFFFF_FFFF) {
        let src = format!("pure fn f() -> u64 {{ 0x{:X} }}", n);
        let result = parser().parse(&src);
        prop_assert!(result.is_ok(), "Failed for 0x{:X}: {:?}", n, result.err());
    }

    #[test]
    fn fuzz_binary_literals(n in 0u32..0xFFFF) {
        let src = format!("pure fn f() -> u32 {{ 0b{:b} }}", n);
        let result = parser().parse(&src);
        prop_assert!(result.is_ok(), "Failed for 0b{:b}: {:?}", n, result.err());
    }

    #[test]
    fn fuzz_underscore_numbers(n in 1000u64..999_999_999) {
        let s = n.to_string();
        let with_underscores = s.chars().enumerate()
            .map(|(i, c)| if i > 0 && (s.len() - i) % 3 == 0 { format!("_{}", c) } else { c.to_string() })
            .collect::<String>();
        let src = format!("pure fn f() -> u64 {{ {} }}", with_underscores);
        let result = parser().parse(&src);
        prop_assert!(result.is_ok(), "Failed for {}: {:?}", with_underscores, result.err());
    }

    #[test]
    fn fuzz_arithmetic_chains(
        ops in prop::collection::vec(prop_oneof![Just("+"), Just("-"), Just("*")], 1..=5),
        vals in prop::collection::vec(1u64..100, 2..=6),
    ) {
        if vals.len() > ops.len() {
            let mut expr = vals[0].to_string();
            for (i, op) in ops.iter().enumerate() {
                if i + 1 < vals.len() {
                    expr = format!("{} {} {}", expr, op, vals[i + 1]);
                }
            }
            let src = format!("pure fn f() -> u64 {{ {} }}", expr);
            let result = parser().parse(&src);
            prop_assert!(result.is_ok(), "Failed for expr '{}': {:?}", expr, result.err());
        }
    }
}
