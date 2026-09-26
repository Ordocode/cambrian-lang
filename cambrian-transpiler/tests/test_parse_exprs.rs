// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

use cambrian_transpiler::ast::*;
use cambrian_transpiler::ExprParser;
use cambrian_transpiler::BlockBodyParser;

use cambrian_core::U256;
// ===== 1.3.2: Literals =====

#[test]
fn parse_int_literal() {
    let result = ExprParser::new().parse("42").unwrap();
    assert_eq!(result, Expr::IntLiteral(U256::from_u128(42)));
}

#[test]
fn parse_int_literal_zero() {
    let result = ExprParser::new().parse("0").unwrap();
    assert_eq!(result, Expr::IntLiteral(U256::ZERO));
}

#[test]
fn parse_hex_literal() {
    let result = ExprParser::new().parse("0xFF").unwrap();
    assert_eq!(result, Expr::IntLiteral(U256::from_hex_digits("FF").unwrap()));
}

#[test]
fn parse_hex_literal_long() {
    let result = ExprParser::new().parse("0xFFFFFFFF").unwrap();
    assert_eq!(result, Expr::IntLiteral(U256::from_hex_digits("FFFFFFFF").unwrap()));
}

#[test]
fn parse_string_literal() {
    let result = ExprParser::new().parse(r#""hello""#).unwrap();
    assert_eq!(result, Expr::StringLiteral("hello".into()));
}

#[test]
fn parse_bool_true() {
    let result = ExprParser::new().parse("true").unwrap();
    assert_eq!(result, Expr::BoolLiteral(true));
}

#[test]
fn parse_bool_false() {
    let result = ExprParser::new().parse("false").unwrap();
    assert_eq!(result, Expr::BoolLiteral(false));
}

#[test]
fn parse_empty_collection() {
    let result = ExprParser::new().parse("{}").unwrap();
    assert_eq!(result, Expr::EmptyCollection);
}

// ===== 1.3.3: Basic expressions =====

#[test]
fn parse_ident() {
    let result = ExprParser::new().parse("x").unwrap();
    assert_eq!(result, Expr::Ident("x".into()));
}

#[test]
fn parse_ident_with_prefix() {
    let result = ExprParser::new().parse("m_owner_key").unwrap();
    assert_eq!(result, Expr::Ident("m_owner_key".into()));
}

#[test]
fn parse_add() {
    let result = ExprParser::new().parse("a + b").unwrap();
    assert_eq!(result, Expr::BinOp(
        Box::new(Expr::Ident("a".into())),
        BinOp::Add,
        Box::new(Expr::Ident("b".into())),
    ));
}

#[test]
fn parse_precedence_mul_over_add() {
    // a + b * c  =>  a + (b * c)
    let result = ExprParser::new().parse("a + b * c").unwrap();
    assert_eq!(result, Expr::BinOp(
        Box::new(Expr::Ident("a".into())),
        BinOp::Add,
        Box::new(Expr::BinOp(
            Box::new(Expr::Ident("b".into())),
            BinOp::Mul,
            Box::new(Expr::Ident("c".into())),
        )),
    ));
}

#[test]
fn parse_precedence_comparison_over_logical() {
    // a > 0 && b < 10  =>  (a > 0) && (b < 10)
    let result = ExprParser::new().parse("a > 0 && b < 10").unwrap();
    assert_eq!(result, Expr::BinOp(
        Box::new(Expr::BinOp(
            Box::new(Expr::Ident("a".into())),
            BinOp::Gt,
            Box::new(Expr::IntLiteral(U256::ZERO)),
        )),
        BinOp::And,
        Box::new(Expr::BinOp(
            Box::new(Expr::Ident("b".into())),
            BinOp::Lt,
            Box::new(Expr::IntLiteral(U256::from_u128(10))),
        )),
    ));
}

#[test]
fn parse_bitwise_shift() {
    let result = ExprParser::new().parse("mask >> 8").unwrap();
    assert_eq!(result, Expr::BinOp(
        Box::new(Expr::Ident("mask".into())),
        BinOp::Shr,
        Box::new(Expr::IntLiteral(U256::from_u128(8))),
    ));
}

#[test]
fn parse_bitwise_and() {
    let result = ExprParser::new().parse("x & 0xFF").unwrap();
    assert_eq!(result, Expr::BinOp(
        Box::new(Expr::Ident("x".into())),
        BinOp::BitAnd,
        Box::new(Expr::IntLiteral(U256::from_hex_digits("FF").unwrap())),
    ));
}

#[test]
fn parse_bitwise_or() {
    let result = ExprParser::new().parse("mask | flag").unwrap();
    assert_eq!(result, Expr::BinOp(
        Box::new(Expr::Ident("mask".into())),
        BinOp::BitOr,
        Box::new(Expr::Ident("flag".into())),
    ));
}

#[test]
fn parse_not_equal() {
    let result = ExprParser::new().parse("a != 0").unwrap();
    assert_eq!(result, Expr::BinOp(
        Box::new(Expr::Ident("a".into())),
        BinOp::Ne,
        Box::new(Expr::IntLiteral(U256::ZERO)),
    ));
}

#[test]
fn parse_unary_not() {
    let result = ExprParser::new().parse("!flag").unwrap();
    assert_eq!(result, Expr::UnaryOp(UnaryOp::Not, Box::new(Expr::Ident("flag".into()))));
}

#[test]
fn parse_unary_deref() {
    let result = ExprParser::new().parse("*owner").unwrap();
    assert_eq!(result, Expr::UnaryOp(UnaryOp::Deref, Box::new(Expr::Ident("owner".into()))));
}

#[test]
fn parse_parens() {
    let result = ExprParser::new().parse("(a + b) * c").unwrap();
    assert_eq!(result, Expr::BinOp(
        Box::new(Expr::BinOp(
            Box::new(Expr::Ident("a".into())),
            BinOp::Add,
            Box::new(Expr::Ident("b".into())),
        )),
        BinOp::Mul,
        Box::new(Expr::Ident("c".into())),
    ));
}

// ===== 1.3.4: Complex expressions =====

#[test]
fn parse_field_access() {
    let result = ExprParser::new().parse("txn.index").unwrap();
    assert_eq!(result, Expr::FieldAccess(
        Box::new(Expr::Ident("txn".into())),
        "index".into(),
    ));
}

#[test]
fn parse_chained_field_access() {
    let result = ExprParser::new().parse("a.b.c").unwrap();
    assert_eq!(result, Expr::FieldAccess(
        Box::new(Expr::FieldAccess(
            Box::new(Expr::Ident("a".into())),
            "b".into(),
        )),
        "c".into(),
    ));
}

#[test]
fn parse_index() {
    let result = ExprParser::new().parse("owners[0]").unwrap();
    assert_eq!(result, Expr::Index(
        Box::new(Expr::Ident("owners".into())),
        Box::new(Expr::IntLiteral(U256::ZERO)),
    ));
}

#[test]
fn parse_index_with_expr() {
    let result = ExprParser::new().parse("m_custodians[msg::pubkey]").unwrap();
    assert_eq!(result, Expr::Index(
        Box::new(Expr::Ident("m_custodians".into())),
        Box::new(Expr::MsgField("pubkey".into())),
    ));
}

#[test]
fn parse_method_call_no_args() {
    let result = ExprParser::new().parse("owners.len()").unwrap();
    assert_eq!(result, Expr::MethodCall(
        Box::new(Expr::Ident("owners".into())),
        "len".into(),
        vec![],
    ));
}

#[test]
fn parse_method_call_with_arg() {
    let result = ExprParser::new().parse("acc.exists(*owner)").unwrap();
    assert_eq!(result, Expr::MethodCall(
        Box::new(Expr::Ident("acc".into())),
        "exists".into(),
        vec![Expr::UnaryOp(UnaryOp::Deref, Box::new(Expr::Ident("owner".into())))],
    ));
}

#[test]
fn parse_method_call_two_args() {
    let result = ExprParser::new().parse("acc.insert(*owner, true)").unwrap();
    assert_eq!(result, Expr::MethodCall(
        Box::new(Expr::Ident("acc".into())),
        "insert".into(),
        vec![
            Expr::UnaryOp(UnaryOp::Deref, Box::new(Expr::Ident("owner".into()))),
            Expr::BoolLiteral(true),
        ],
    ));
}

#[test]
fn parse_fn_call() {
    let result = ExprParser::new().parse("check_bit(mask, index)").unwrap();
    assert_eq!(result, Expr::FnCall(
        "check_bit".into(),
        vec![Expr::Ident("mask".into()), Expr::Ident("index".into())],
    ));
}

#[test]
fn parse_fn_call_no_args() {
    let result = ExprParser::new().parse("foo()").unwrap();
    assert_eq!(result, Expr::FnCall("foo".into(), vec![]));
}

// ===== 1.3.5: Control flow =====

// if/let/blocks are parsed via BlockBodyParser (they only appear inside { })

#[test]
fn parse_if_else() {
    let result = BlockBodyParser::new().parse("if a <= b { a } else { b }").unwrap();
    assert_eq!(result, Expr::If(
        Box::new(Expr::BinOp(
            Box::new(Expr::Ident("a".into())),
            BinOp::Le,
            Box::new(Expr::Ident("b".into())),
        )),
        Box::new(Expr::Ident("a".into())),
        Some(Box::new(Expr::Ident("b".into()))),
    ));
}

#[test]
fn parse_if_without_else() {
    let result = BlockBodyParser::new().parse("if x > 0 { x }").unwrap();
    assert_eq!(result, Expr::If(
        Box::new(Expr::BinOp(
            Box::new(Expr::Ident("x".into())),
            BinOp::Gt,
            Box::new(Expr::IntLiteral(U256::ZERO)),
        )),
        Box::new(Expr::Ident("x".into())),
        None,
    ));
}

#[test]
fn parse_block_with_let() {
    let result = BlockBodyParser::new().parse("let x = 5; x").unwrap();
    assert_eq!(result, Expr::Let(
        Pattern::Ident("x".into()),
        Box::new(Expr::IntLiteral(U256::from_u128(5))),
        Box::new(Expr::Ident("x".into())),
    ));
}

#[test]
fn parse_let_tuple_destructure() {
    let result = BlockBodyParser::new().parse("let (a, b) = pair; a").unwrap();
    assert_eq!(result, Expr::Let(
        Pattern::Tuple(vec![Pattern::Ident("a".into()), Pattern::Ident("b".into())]),
        Box::new(Expr::Ident("pair".into())),
        Box::new(Expr::Ident("a".into())),
    ));
}

// ===== Wrapping arithmetic =====

#[test]
fn parse_wrapping_add() {
    let result = ExprParser::new().parse("a +% b").unwrap();
    assert_eq!(result, Expr::BinOp(
        Box::new(Expr::Ident("a".into())),
        BinOp::WrappingAdd,
        Box::new(Expr::Ident("b".into())),
    ));
}

#[test]
fn parse_wrapping_sub() {
    let result = ExprParser::new().parse("a -% b").unwrap();
    assert_eq!(result, Expr::BinOp(
        Box::new(Expr::Ident("a".into())),
        BinOp::WrappingSub,
        Box::new(Expr::Ident("b".into())),
    ));
}

#[test]
fn parse_wrapping_mul() {
    let result = ExprParser::new().parse("a *% b").unwrap();
    assert_eq!(result, Expr::BinOp(
        Box::new(Expr::Ident("a".into())),
        BinOp::WrappingMul,
        Box::new(Expr::Ident("b".into())),
    ));
}

// ===== Range and for =====

#[test]
fn parse_range_expr() {
    let result = ExprParser::new().parse("0..10").unwrap();
    assert_eq!(result, Expr::Range(
        Box::new(Expr::IntLiteral(U256::ZERO)),
        Box::new(Expr::IntLiteral(U256::from_u128(10))),
    ));
}

#[test]
fn parse_for_expr() {
    let result = BlockBodyParser::new().parse("for i in 0..n { i * 2 }").unwrap();
    if let Expr::For(pat, iter, body) = result {
        assert_eq!(pat, Pattern::Ident("i".into()));
        assert!(matches!(*iter, Expr::Range(_, _)));
        assert!(matches!(*body, Expr::BinOp(_, BinOp::Mul, _)));
    } else {
        panic!("Expected For expression");
    }
}

#[test]
fn parse_for_over_collection() {
    let result = BlockBodyParser::new().parse("for x in items { x + 1 }").unwrap();
    if let Expr::For(pat, iter, body) = result {
        assert_eq!(pat, Pattern::Ident("x".into()));
        assert_eq!(*iter, Expr::Ident("items".into()));
        assert!(matches!(*body, Expr::BinOp(_, BinOp::Add, _)));
    } else {
        panic!("Expected For expression");
    }
}

// ===== Option destructuring =====

#[test]
fn parse_let_some_destructure() {
    let result = BlockBodyParser::new().parse("let some(x) = opt; x").unwrap();
    assert_eq!(result, Expr::Let(
        Pattern::Some(Box::new(Pattern::Ident("x".into()))),
        Box::new(Expr::Ident("opt".into())),
        Box::new(Expr::Ident("x".into())),
    ));
}

#[test]
fn parse_let_none_pattern() {
    let result = BlockBodyParser::new().parse("let none = opt; 0").unwrap();
    assert_eq!(result, Expr::Let(
        Pattern::None,
        Box::new(Expr::Ident("opt".into())),
        Box::new(Expr::IntLiteral(U256::ZERO)),
    ));
}

#[test]
fn parse_match_with_some_none() {
    let result = BlockBodyParser::new().parse(
        "match opt { some(x) => x, none => 0 }"
    ).unwrap();
    if let Expr::Match(_, arms) = result {
        assert_eq!(arms.len(), 2);
        assert_eq!(arms[0].pattern, MatchPattern::Some(Pattern::Ident("x".into())));
        assert_eq!(arms[1].pattern, MatchPattern::None);
    } else {
        panic!("Expected Match");
    }
}

// ===== 1.3.6: Closures =====

#[test]
fn parse_simple_closure() {
    let result = ExprParser::new().parse("|x| x").unwrap();
    assert_eq!(result, Expr::Closure(
        vec![Pattern::Ident("x".into())],
        Box::new(Expr::Ident("x".into())),
    ));
}

#[test]
fn parse_closure_two_params() {
    let result = ExprParser::new().parse("|a, b| a + b").unwrap();
    assert_eq!(result, Expr::Closure(
        vec![Pattern::Ident("a".into()), Pattern::Ident("b".into())],
        Box::new(Expr::BinOp(
            Box::new(Expr::Ident("a".into())),
            BinOp::Add,
            Box::new(Expr::Ident("b".into())),
        )),
    ));
}

#[test]
fn parse_closure_with_destructure() {
    let result = ExprParser::new().parse("|(_, owner)| *owner != 0").unwrap();
    assert_eq!(result, Expr::Closure(
        vec![Pattern::Tuple(vec![Pattern::Wildcard, Pattern::Ident("owner".into())])],
        Box::new(Expr::BinOp(
            Box::new(Expr::UnaryOp(UnaryOp::Deref, Box::new(Expr::Ident("owner".into())))),
            BinOp::Ne,
            Box::new(Expr::IntLiteral(U256::ZERO)),
        )),
    ));
}

// ===== 1.3.7: Special expressions =====

#[test]
fn parse_temporal_ref() {
    let result = ExprParser::new().parse("^m_custodian_count").unwrap();
    assert_eq!(result, Expr::TemporalRef("m_custodian_count".into()));
}

#[test]
fn parse_macro_ref() {
    let result = ExprParser::new().parse("@cleaned_transactions()").unwrap();
    assert_eq!(result, Expr::MacroRef("cleaned_transactions".into(), vec![]));
}

#[test]
fn parse_macro_ref_with_arg() {
    let result = ExprParser::new().parse("@custodian_not_confirmed(transaction_id)").unwrap();
    assert_eq!(result, Expr::MacroRef(
        "custodian_not_confirmed".into(),
        vec![Expr::Ident("transaction_id".into())],
    ));
}

#[test]
fn parse_msg_field_sender() {
    let result = ExprParser::new().parse("msg::sender").unwrap();
    assert_eq!(result, Expr::MsgField("sender".into()));
}

#[test]
fn parse_msg_field_timestamp() {
    let result = ExprParser::new().parse("msg::timestamp").unwrap();
    assert_eq!(result, Expr::MsgField("timestamp".into()));
}

#[test]
fn parse_msg_field_pubkey() {
    let result = ExprParser::new().parse("msg::pubkey").unwrap();
    assert_eq!(result, Expr::MsgField("pubkey".into()));
}

#[test]
fn parse_msg_field_int() {
    let result = ExprParser::new().parse("msg::int").unwrap();
    assert_eq!(result, Expr::MsgField("int".into()));
}

#[test]
fn parse_msg_field_ext() {
    let result = ExprParser::new().parse("msg::ext").unwrap();
    assert_eq!(result, Expr::MsgField("ext".into()));
}

#[test]
fn parse_msg_field_body() {
    let result = ExprParser::new().parse("msg::body").unwrap();
    assert_eq!(result, Expr::MsgField("body".into()));
}

#[test]
fn parse_sys_field_now() {
    let result = ExprParser::new().parse("sys::now").unwrap();
    assert_eq!(result, Expr::SysField("now".into()));
}

#[test]
fn parse_sys_field_address() {
    let result = ExprParser::new().parse("sys::address").unwrap();
    assert_eq!(result, Expr::SysField("address".into()));
}

#[test]
fn parse_sys_field_logicaltime() {
    let result = ExprParser::new().parse("sys::logicaltime").unwrap();
    assert_eq!(result, Expr::SysField("logicaltime".into()));
}

#[test]
fn parse_sys_field_rnd_seed() {
    let result = ExprParser::new().parse("sys::rnd_seed").unwrap();
    assert_eq!(result, Expr::SysField("rnd_seed".into()));
}

#[test]
fn parse_cast() {
    let result = ExprParser::new().parse("i as uint8").unwrap();
    assert_eq!(result, Expr::Cast(
        Box::new(Expr::Ident("i".into())),
        Type::Simple("uint8".into()),
    ));
}

#[test]
fn parse_tuple_expr() {
    let result = ExprParser::new().parse("(a, b)").unwrap();
    assert_eq!(result, Expr::Tuple(vec![
        Expr::Ident("a".into()),
        Expr::Ident("b".into()),
    ]));
}

// Record construct/update parsed via BlockBodyParser (only in block/let/closure contexts)

#[test]
fn parse_record_construct() {
    let result = BlockBodyParser::new().parse("CustodianInfo { index: *index, pubkey: *pubkey }").unwrap();
    assert_eq!(result, Expr::RecordConstruct(
        "CustodianInfo".into(),
        vec![
            ("index".into(), Expr::UnaryOp(UnaryOp::Deref, Box::new(Expr::Ident("index".into())))),
            ("pubkey".into(), Expr::UnaryOp(UnaryOp::Deref, Box::new(Expr::Ident("pubkey".into())))),
        ],
    ));
}

#[test]
fn parse_record_update() {
    let result = BlockBodyParser::new().parse("let x = txn { signs_received: txn.signs_received + 1 }; x").unwrap();
    match result {
        Expr::Let(_, val, _) => {
            assert_eq!(*val, Expr::RecordUpdate(
                Box::new(Expr::Ident("txn".into())),
                vec![
                    ("signs_received".into(), Expr::BinOp(
                        Box::new(Expr::FieldAccess(Box::new(Expr::Ident("txn".into())), "signs_received".into())),
                        BinOp::Add,
                        Box::new(Expr::IntLiteral(U256::from_u128(1))),
                    )),
                ],
            ));
        }
        other => panic!("Expected Let, got {:?}", other),
    }
}

// ===== Phase 5D Group 1: New literals =====

#[test]
fn parse_underscore_in_number() {
    let result = ExprParser::new().parse("1_000_000").unwrap();
    assert_eq!(result, Expr::IntLiteral(U256::from_u128(1000000)));
}

#[test]
fn parse_underscore_in_number_small() {
    let result = ExprParser::new().parse("10_000").unwrap();
    assert_eq!(result, Expr::IntLiteral(U256::from_u128(10000)));
}

#[test]
fn parse_binary_literal() {
    let result = ExprParser::new().parse("0b101010").unwrap();
    assert_eq!(result, Expr::IntLiteral(U256::from_u128(42)));
}

#[test]
fn parse_binary_literal_with_underscores() {
    let result = ExprParser::new().parse("0b1111_0000").unwrap();
    assert_eq!(result, Expr::IntLiteral(U256::from_u128(240)));
}

#[test]
fn parse_array_literal_empty() {
    let result = ExprParser::new().parse("array()").unwrap();
    assert_eq!(result, Expr::ArrayLit(vec![]));
}

#[test]
fn parse_array_literal_ints() {
    let result = ExprParser::new().parse("array(1, 2, 3)").unwrap();
    assert_eq!(result, Expr::ArrayLit(vec![
        Expr::IntLiteral(U256::from_u128(1)),
        Expr::IntLiteral(U256::from_u128(2)),
        Expr::IntLiteral(U256::from_u128(3)),
    ]));
}

#[test]
fn parse_array_literal_strings() {
    let result = ExprParser::new().parse(r#"array("a", "b")"#).unwrap();
    assert_eq!(result, Expr::ArrayLit(vec![
        Expr::StringLiteral("a".to_string()),
        Expr::StringLiteral("b".to_string()),
    ]));
}

#[test]
fn parse_bytes_literal() {
    let result = ExprParser::new().parse(r#"b"hello""#).unwrap();
    assert_eq!(result, Expr::BytesLiteral(b"hello".to_vec()));
}

#[test]
fn parse_bytes_literal_empty() {
    let result = ExprParser::new().parse(r#"b"""#).unwrap();
    assert_eq!(result, Expr::BytesLiteral(vec![]));
}

// ===== Complex expressions from multisig.cam =====

#[test]
fn parse_complex_bitwise() {
    // (mask >> (8 * index)) & 0xFF
    let result = ExprParser::new().parse("(mask >> (8 * index)) & 0xFF").unwrap();
    match result {
        Expr::BinOp(_, BinOp::BitAnd, _) => {},
        other => panic!("Expected BitAnd at top level, got {:?}", other),
    }
}

#[test]
fn parse_method_chain_iter_filter() {
    let result = ExprParser::new().parse("owners.iter().filter(|x| *x != 0)").unwrap();
    match &result {
        Expr::MethodCall(recv, method, args) => {
            assert_eq!(method, "filter");
            assert_eq!(args.len(), 1);
            match recv.as_ref() {
                Expr::MethodCall(_, m, _) => assert_eq!(m, "iter"),
                other => panic!("Expected MethodCall(iter), got {:?}", other),
            }
        }
        other => panic!("Expected MethodCall, got {:?}", other),
    }
}

// ===== Gap 3: Unary Neg operator =====

#[test]
fn parse_unary_neg_ident() {
    let result = ExprParser::new().parse("-x").unwrap();
    match &result {
        Expr::UnaryOp(UnaryOp::Neg, inner) => {
            assert_eq!(**inner, Expr::Ident("x".into()));
        }
        other => panic!("Expected UnaryOp(Neg, _), got {:?}", other),
    }
}

#[test]
fn parse_unary_neg_literal() {
    let result = ExprParser::new().parse("-42").unwrap();
    match &result {
        Expr::UnaryOp(UnaryOp::Neg, inner) => {
            assert_eq!(**inner, Expr::IntLiteral(U256::from_u128(42)));
        }
        other => panic!("Expected UnaryOp(Neg, _), got {:?}", other),
    }
}

#[test]
fn parse_unary_neg_paren() {
    let result = ExprParser::new().parse("-(x + 1)").unwrap();
    match &result {
        Expr::UnaryOp(UnaryOp::Neg, _) => {}
        other => panic!("Expected UnaryOp(Neg, _), got {:?}", other),
    }
}

#[test]
fn parse_double_neg() {
    let result = ExprParser::new().parse("--x").unwrap();
    match &result {
        Expr::UnaryOp(UnaryOp::Neg, inner) => {
            match inner.as_ref() {
                Expr::UnaryOp(UnaryOp::Neg, _) => {}
                other => panic!("Expected inner Neg, got {:?}", other),
            }
        }
        other => panic!("Expected UnaryOp(Neg, _), got {:?}", other),
    }
}

#[test]
fn parse_neg_in_binop() {
    let result = ExprParser::new().parse("a + -b").unwrap();
    match &result {
        Expr::BinOp(_, BinOp::Add, rhs) => {
            match rhs.as_ref() {
                Expr::UnaryOp(UnaryOp::Neg, _) => {}
                other => panic!("Expected UnaryOp(Neg, _), got {:?}", other),
            }
        }
        other => panic!("Expected BinOp, got {:?}", other),
    }
}
