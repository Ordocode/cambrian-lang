// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

use cambrian_transpiler::ast::*;
use cambrian_transpiler::TypeParser;

#[test]
fn parse_simple_type_bool() {
    let result = TypeParser::new().parse("bool").unwrap();
    assert_eq!(result, Type::Simple("bool".into()));
}

#[test]
fn parse_simple_type_uint256() {
    let result = TypeParser::new().parse("uint256").unwrap();
    assert_eq!(result, Type::Simple("uint256".into()));
}

#[test]
fn parse_simple_type_address() {
    let result = TypeParser::new().parse("address").unwrap();
    assert_eq!(result, Type::Simple("address".into()));
}

#[test]
fn parse_generic_type_array() {
    let result = TypeParser::new().parse("Vec<uint256>").unwrap();
    assert_eq!(result, Type::Generic("Vec".into(), vec![Type::Simple("uint256".into())]));
}

#[test]
fn parse_generic_type_mapping() {
    let result = TypeParser::new().parse("mapping<uint64, Transaction>").unwrap();
    assert_eq!(result, Type::Generic(
        "mapping".into(),
        vec![Type::Simple("uint64".into()), Type::Simple("Transaction".into())]
    ));
}

#[test]
fn parse_nested_generic_type() {
    let result = TypeParser::new().parse("mapping<uint32, uint32>").unwrap();
    assert_eq!(result, Type::Generic(
        "mapping".into(),
        vec![Type::Simple("uint32".into()), Type::Simple("uint32".into())]
    ));
}

#[test]
fn parse_tuple_type() {
    let result = TypeParser::new().parse("(uint8, uint8, uint64)").unwrap();
    assert_eq!(result, Type::Tuple(vec![
        Type::Simple("uint8".into()),
        Type::Simple("uint8".into()),
        Type::Simple("uint64".into()),
    ]));
}

#[test]
fn parse_tuple_type_with_generic() {
    let result = TypeParser::new().parse("(mapping<uint64, Transaction>, uint256)").unwrap();
    assert_eq!(result, Type::Tuple(vec![
        Type::Generic("mapping".into(), vec![
            Type::Simple("uint64".into()),
            Type::Simple("Transaction".into()),
        ]),
        Type::Simple("uint256".into()),
    ]));
}
