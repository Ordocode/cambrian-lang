// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Tests for the `property` construct: parsing, validation rules
//! (T11 / T17 / T18), and the desugaring pass that lowers a property into
//! concrete `test` / `fuzz` declarations.

use cambrian_core::U256;
use cambrian_transpiler::ast::{self, InstanceArg, PropertyInstanceKind, TestStep};
use cambrian_transpiler::codegen::{EvmSolidityBackend, LeanBackend, OutputBackend};
use cambrian_transpiler::desugar::desugar_properties;
use cambrian_transpiler::validate::{validate, Severity};
use cambrian_transpiler::ProgramParser;

const COUNTER: &str = r#"
entity Counter {
    routes {
        increment(amount: u64) => []
        reset() => []
    }
    m_count: u64 {
        in increment(amount) => m_count + amount
        in reset() => 0
    }
}
"#;

fn parse(extra: &str) -> ast::Program {
    let src = format!("{}\n{}", COUNTER, extra);
    let mut p = ProgramParser::new().parse(&src).expect("must parse");
    ast::normalize_program_types(&mut p);
    p
}

fn codes(prog: &ast::Program) -> Vec<String> {
    validate(prog).into_iter().map(|d| d.code.to_string()).collect()
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

#[test]
fn parses_property_with_params_before_for() {
    let prog = parse(
        r#"
property "incr" (amount: u64) for Counter with { m_count: 0 } {
    call increment(amount)
    expect state { m_count: amount }

    test "four" { amount: 4 }
    fuzz "small" { amount in 0..1000 }
}
"#,
    );
    assert_eq!(prog.properties.len(), 1);
    let p = &prog.properties[0];
    assert_eq!(p.name, "incr");
    assert_eq!(p.entity_name, "Counter");
    assert_eq!(p.params.len(), 1);
    assert_eq!(p.params[0].name, "amount");
    assert_eq!(p.init_state.len(), 1);
    assert_eq!(p.instances.len(), 2);
    assert_eq!(p.instances[0].kind, PropertyInstanceKind::Test);
    assert_eq!(p.instances[1].kind, PropertyInstanceKind::Fuzz);
    // No top-level fuzz collection populated at parse time.
    assert!(prog.fuzz_tests.is_empty());
}

#[test]
fn parses_property_instance_attributes() {
    let prog = parse(
        r#"
property "incr" (amount: u64) for Counter {
    call increment(amount)
    expect state { m_count: amount }

    #[runs(2000)] fuzz "wide" { amount in 0..=1000 } with { m_count: 5 }
    #[skip_from] test "z" { amount: 0 }
}
"#,
    );
    let p = &prog.properties[0];
    let fuzz = &p.instances[0];
    assert_eq!(fuzz.runs, Some(2000));
    assert_eq!(fuzz.init_state.len(), 1);
    if let InstanceArg::Range { inclusive, .. } = &fuzz.bindings[0].1 {
        assert!(*inclusive, "0..=1000 is inclusive");
    } else {
        panic!("expected a range binding");
    }
    let test = &p.instances[1];
    assert!(test.skip_from);
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

#[test]
fn property_with_no_instances_is_valid() {
    let prog = parse(
        r#"
property "incr" (amount: u64) for Counter with { m_count: 0 } {
    call increment(amount)
    expect state { m_count: amount }
}
"#,
    );
    assert!(validate(&prog).iter().all(|d| !matches!(d.severity, Severity::Error)));
}

#[test]
fn t11_rejects_bound_in_property_body() {
    let prog = parse(
        r#"
property "incr" (amount: u64) for Counter {
    bound amount in 0..10
    call increment(amount)
    expect state { m_count: amount }
}
"#,
    );
    assert!(codes(&prog).contains(&"T11".to_string()),
        "bound in property body must raise T11");
}

#[test]
fn t11_rejects_binding_unknown_param() {
    let prog = parse(
        r#"
property "incr" (amount: u64) for Counter {
    call increment(amount)
    expect state { m_count: amount }

    fuzz { nope in 0..10 }
}
"#,
    );
    assert!(codes(&prog).contains(&"T11".to_string()),
        "binding an undeclared parameter must raise T11");
}

#[test]
fn t17_rejects_range_in_test_instance() {
    let prog = parse(
        r#"
property "incr" (amount: u64) for Counter {
    call increment(amount)
    expect state { m_count: amount }

    test { amount in 0..10 }
}
"#,
    );
    assert!(codes(&prog).contains(&"T17".to_string()),
        "a range binding in a test instance must raise T17");
}

#[test]
#[test]
fn t24_rejects_empty_exclusive_fuzz_range() {
    let prog = parse(
        r#"
property "incr" (amount: u64) for Counter {
    call increment(amount)
    expect state { m_count: amount }

    fuzz { amount in 5..5 }
}
"#,
    );
    assert!(
        codes(&prog).contains(&"T31".to_string()),
        "exclusive 5..5 must raise T31"
    );
}

#[test]
fn t24_rejects_inverted_fuzz_range() {
    let prog = parse(
        r#"
property "incr" (amount: u64) for Counter {
    call increment(amount)
    expect state { m_count: amount }

    fuzz { amount in 10..5 }
}
"#,
    );
    assert!(
        codes(&prog).contains(&"T31".to_string()),
        "10..5 must raise T31"
    );
}

#[test]
fn t24_allows_valid_fuzz_range() {
    let prog = parse(
        r#"
property "incr" (amount: u64) for Counter {
    call increment(amount)
    expect state { m_count: amount }

    fuzz { amount in 0..10 }
}
"#,
    );
    assert!(
        !codes(&prog).contains(&"T31".to_string()),
        "0..10 must not raise T31"
    );
}

#[test]
fn t24_rejects_empty_u256_max_exclusive_self() {
    let prog = parse(
        r#"
property "wide" (amount: U256) for Counter {
    call increment(0)
    expect state { m_count: 0 }

    fuzz { amount in U256::MAX..U256::MAX }
}
"#,
    );
    assert!(
        codes(&prog).contains(&"T31".to_string()),
        "MAX..MAX exclusive must raise T31"
    );
}

#[test]
fn t17_rejects_concrete_in_fuzz_instance() {
    let prog = parse(
        r#"
property "incr" (amount: u64) for Counter {
    call increment(amount)
    expect state { m_count: amount }

    fuzz { amount: 4 }
}
"#,
    );
    assert!(codes(&prog).contains(&"T17".to_string()),
        "a concrete binding in a fuzz instance must raise T17");
}

#[test]
fn t18_requires_test_to_bind_all_params() {
    let prog = parse(
        r#"
property "two" (a: u64, b: u64) for Counter {
    call increment(a)
    expect state { m_count: a }

    test { a: 1 }
}
"#,
    );
    assert!(codes(&prog).contains(&"T18".to_string()),
        "a test instance leaving a parameter unbound must raise T18");
}

// ---------------------------------------------------------------------------
// Desugaring
// ---------------------------------------------------------------------------

#[test]
fn desugar_fuzz_instance_synthesizes_bound_step() {
    let mut prog = parse(
        r#"
property "incr" (amount: u64) for Counter with { m_count: 0 } {
    assume amount % 2 == 0
    call increment(amount)
    expect state { m_count: amount }

    fuzz "small" { amount in 0..1000 }
}
"#,
    );
    desugar_properties(&mut prog);
    assert_eq!(prog.fuzz_tests.len(), 1);
    let f = &prog.fuzz_tests[0];
    assert_eq!(f.params.len(), 1);
    assert_eq!(
        f.init_state,
        vec![("m_count".to_string(), ast::Expr::IntLiteral(U256::ZERO))]
    );
    // First step is the synthesized bound, then the assume + call survive.
    assert!(matches!(&f.body[0], TestStep::Bound { var, .. } if var == "amount"));
    assert!(f.body.iter().any(|s| matches!(s, TestStep::Assume { .. })));
    assert!(f.body.iter().any(|s| matches!(s, TestStep::Call { route, .. } if route == "increment")));
}

#[test]
fn desugar_test_instance_prepends_let_and_drops_assume() {
    let mut prog = parse(
        r#"
property "incr" (amount: u64) for Counter with { m_count: 0 } {
    assume amount % 2 == 0
    call increment(amount)
    expect state { m_count: amount }

    test "four" { amount: 4 }
}
"#,
    );
    desugar_properties(&mut prog);
    assert_eq!(prog.tests.len(), 1);
    let t = &prog.tests[0];
    // Concrete value becomes a `let`, and the `assume` precondition is
    // dropped (illegal / meaningless inside a concrete test).
    assert!(matches!(&t.body[0], TestStep::Let { name, .. } if name == "amount"));
    assert!(!t.body.iter().any(|s| matches!(s, TestStep::Assume { .. })));
    assert!(t.body.iter().any(|s| matches!(s, TestStep::Call { route, .. } if route == "increment")));
}

#[test]
fn desugar_auto_fuzz_when_params_and_no_instances() {
    let mut prog = parse(
        r#"
property "incr" (amount: u64) for Counter {
    call increment(amount)
    expect state { m_count: amount }
}
"#,
    );
    desugar_properties(&mut prog);
    assert_eq!(prog.fuzz_tests.len(), 1, "a parameterised property without instances auto-fuzzes");
    assert!(prog.tests.is_empty());
    // No sampling bound was synthesized (full-range fuzz).
    assert!(!prog.fuzz_tests[0].body.iter().any(|s| matches!(s, TestStep::Bound { .. })));
}

#[test]
fn desugar_auto_test_when_no_params_and_no_instances() {
    let mut prog = parse(
        r#"
property "reset is idempotent" for Counter with { m_count: 7 } {
    call reset()
    expect state { m_count: 0 }
}
"#,
    );
    desugar_properties(&mut prog);
    assert_eq!(prog.tests.len(), 1, "a param-free property without instances becomes a deterministic test");
    assert!(prog.fuzz_tests.is_empty());
}

// ---------------------------------------------------------------------------
// Forall ("*") starting state
// ---------------------------------------------------------------------------

#[test]
fn parses_forall_markers() {
    use ast::ForallTarget;
    let prog = parse(
        r#"
property "any start, any sender" (amount: u64) for Counter with { m_count: * } ctx { msg::sender: * } {
    call increment(amount)
    expect state { m_count: m_count }

    fuzz { amount in 0..10 }
}
property "everything quantified" for Counter with { * } {
    call reset()
    expect state { m_count: 0 }
}
"#,
    );
    let p0 = &prog.properties[0];
    assert!(p0.init_state.is_empty(), "no concrete pins on p0");
    // State forall lives in `with`; context forall lives in `ctx`.
    assert_eq!(p0.forall_state.targets.len(), 1);
    assert!(p0.forall_state.targets.contains(&ForallTarget::StateField("m_count".to_string())));
    let ctx_foralls: Vec<(&str, &str)> = p0.context.foralls().collect();
    assert_eq!(ctx_foralls, vec![("msg", "sender")]);
    let p1 = &prog.properties[1];
    assert!(p1.forall_state.all_state, "bare `*` sets all_state");
}

#[test]
fn t19_rejects_forall_unknown_field() {
    let prog = parse(
        r#"
property "p" for Counter with { not_a_member: * } {
    call reset()
    expect state { m_count: 0 }
}
"#,
    );
    assert!(codes(&prog).contains(&"T19".to_string()),
        "forall over a non-member must raise T19");
}

#[test]
fn t20_rejects_unknown_ctx_param() {
    let prog = parse(
        r#"
property "p" for Counter ctx { msg::bogus: * } {
    call reset()
    expect state { m_count: 0 }
}
"#,
    );
    assert!(codes(&prog).contains(&"T20".to_string()),
        "an unknown context param in `ctx` must raise T20");
}

#[test]
fn t22_rejects_context_in_with() {
    let prog = parse(
        r#"
property "p" for Counter with { msg::sender: * } {
    call reset()
    expect state { m_count: 0 }
}
"#,
    );
    assert!(codes(&prog).contains(&"T22".to_string()),
        "context params in `with` must raise T22 (use a `ctx` block)");
}

#[test]
fn t23_rejects_duplicate_ctx_param() {
    let prog = parse(
        r#"
property "p" for Counter ctx { msg::sender: *, msg::sender: 0x1 } {
    call reset()
    expect state { m_count: 0 }
}
"#,
    );
    assert!(codes(&prog).contains(&"T23".to_string()),
        "a context param declared twice in one `ctx` block must raise T23");
}

#[test]
fn t21_rejects_pinned_and_forall() {
    let prog = parse(
        r#"
property "p" for Counter with { m_count: 0, m_count: * } {
    call reset()
    expect state { m_count: 0 }
}
"#,
    );
    assert!(codes(&prog).contains(&"T21".to_string()),
        "a field both pinned and forall-ized must raise T21");
}

#[test]
fn desugar_forall_field_adds_fuzz_param_and_pin() {
    let mut prog = parse(
        r#"
property "reset from any start" for Counter with { m_count: * } {
    call reset()
    expect state { m_count: 0 }
}
"#,
    );
    desugar_properties(&mut prog);
    // forall + no params → auto-fuzz with a synthesized sampled param.
    assert_eq!(prog.fuzz_tests.len(), 1);
    let f = &prog.fuzz_tests[0];
    assert!(f.params.iter().any(|p| p.name == "m_count"),
        "forall state field becomes a fuzz parameter");
    assert!(
        f.init_state.iter().any(|(n, v)| n == "m_count"
            && matches!(v, ast::Expr::Ident(id) if id == "m_count")),
        "forall state field is pinned to the sampled parameter",
    );
}

#[test]
fn desugar_forall_ctx_adds_context_step() {
    let mut prog = parse(
        r#"
property "incr from any sender" (amount: u64) for Counter with { m_count: 0 } ctx { msg::sender: * } {
    call increment(amount)
    expect state { m_count: amount }

    fuzz { amount in 0..10 }
}
"#,
    );
    desugar_properties(&mut prog);
    let f = &prog.fuzz_tests[0];
    assert!(f.params.iter().any(|p| p.name == "msg_sender"),
        "forall ctx param becomes a fuzz parameter");
    assert!(
        f.body.iter().any(|s| matches!(s, TestStep::SetContext { namespace, fields }
            if namespace == "msg" && fields.iter().any(|(n, _)| n == "sender"))),
        "forall ctx param emits a msg context step",
    );
}

#[test]
fn desugar_ctx_pin_emits_concrete_context_step() {
    let mut prog = parse(
        r#"
property "incr from fixed sender" (amount: u64) for Counter with { m_count: 0 } ctx { msg::sender: 0x1234 } {
    call increment(amount)
    expect state { m_count: amount }

    fuzz { amount in 0..10 }
}
"#,
    );
    desugar_properties(&mut prog);
    let f = &prog.fuzz_tests[0];
    // A concrete ctx pin does NOT add a fuzz parameter ...
    assert!(!f.params.iter().any(|p| p.name == "msg_sender"),
        "a concrete ctx pin must not synthesize a fuzz parameter");
    // ... but it does emit a context step with the literal value.
    assert!(
        f.body.iter().any(|s| matches!(s, TestStep::SetContext { namespace, fields }
            if namespace == "msg" && fields.iter().any(|(n, _)| n == "sender"))),
        "a concrete ctx pin emits a msg context step",
    );
}

#[test]
fn desugar_ctx_forall_only_auto_fuzzes() {
    let mut prog = parse(
        r#"
property "any sender" for Counter ctx { msg::sender: * } {
    call reset()
    expect state { m_count: 0 }
}
"#,
    );
    desugar_properties(&mut prog);
    // A context forall with no params / state forall still forces a fuzz.
    assert_eq!(prog.fuzz_tests.len(), 1, "ctx forall forces an auto-fuzz");
    assert!(prog.tests.is_empty());
    let f = &prog.fuzz_tests[0];
    assert!(f.params.iter().any(|p| p.name == "msg_sender"));
}

#[test]
fn desugar_all_state_expands_members() {
    let mut prog = parse(
        r#"
property "all quantified" for Counter with { * } {
    call reset()
    expect state { m_count: 0 }
}
"#,
    );
    desugar_properties(&mut prog);
    let f = &prog.fuzz_tests[0];
    assert!(f.params.iter().any(|p| p.name == "m_count"),
        "`with {{ * }}` expands to every state member as a fuzz param");
}

#[test]
fn w8_warns_test_instance_leaving_forall_unpinned() {
    let prog = parse(
        r#"
property "p" (amount: u64) for Counter with { m_count: * } {
    call increment(amount)
    expect state { m_count: m_count }

    test "t" { amount: 1 }
}
"#,
    );
    assert!(codes(&prog).contains(&"W8".to_string()),
        "a test instance leaving a property-level forall field unpinned must warn W8");
}

fn error_codes(prog: &ast::Program) -> Vec<String> {
    validate(prog)
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.code.to_string())
        .collect()
}

const PRED_ENTITY: &str = r#"
entity Counter {
    routes {
        view getCount() -> u64 => [return(m_count)]
        increment(amount: u64) => []
    }
    m_count: u64 {
        in increment(amount) => m_count + amount
    }
}
"#;

fn parse_pred(extra: &str) -> ast::Program {
    let src = format!("{}\n{}", PRED_ENTITY, extra);
    let mut p = ProgramParser::new().parse(&src).expect("must parse");
    ast::normalize_program_types(&mut p);
    p
}

#[test]
fn t38_rejects_non_boolean_expect() {
    let prog = parse_pred(
        r#"
property "bad shape" for Counter {
    call getCount()
    expect (1, 2)
}
"#,
    );
    assert!(
        error_codes(&prog).contains(&"T38".to_string()),
        "tuple expect must be T38, got {:?}",
        error_codes(&prog)
    );
}

#[test]
fn t38_rejects_result_on_void_route() {
    let prog = parse_pred(
        r#"
property "void" for Counter {
    call increment(1)
    expect return > 0
}
"#,
    );
    assert!(
        error_codes(&prog).contains(&"T38".to_string()),
        "result on a void route must be T38, got {:?}",
        error_codes(&prog)
    );
}

#[test]
fn t38_rejects_result_name_clash() {
    let prog = parse_pred(
        r#"
property "clash" (result: u64) for Counter {
    call getCount()
    expect result > 0
}
"#,
    );
    assert!(
        error_codes(&prog).contains(&"T38".to_string()),
        "parameter named result must be T38, got {:?}",
        error_codes(&prog)
    );

    let prog = parse_pred(
        r#"
property "let clash" for Counter {
    let result = 1
    call getCount()
    expect result > 0
}
"#,
    );
    assert!(
        error_codes(&prog).contains(&"T38".to_string()),
        "let named result must be T38, got {:?}",
        error_codes(&prog)
    );

    let src = r#"
entity Box {
    routes { get() -> u64 => [return(result)] }
    result: u64 {}
}
property "member clash" for Box {
    call get()
    expect result > 0
}
"#;
    let mut prog = ProgramParser::new().parse(src).expect("must parse");
    ast::normalize_program_types(&mut prog);
    assert!(
        error_codes(&prog).contains(&"T38".to_string()),
        "member named result must be T38, got {:?}",
        error_codes(&prog)
    );
}

#[test]
fn t5_rejects_predicate_before_call() {
    let prog = parse_pred(
        r#"
property "early" for Counter {
    expect return > 0
    call getCount()
}
"#,
    );
    let errs = error_codes(&prog);
    assert!(errs.contains(&"T5".to_string()), "predicate before call must be T5, got {errs:?}");
    assert!(!errs.contains(&"T38".to_string()), "ordering failure is T5, got {errs:?}");
}

fn t38_message(prog: &ast::Program, needle: &str) {
    let hits: Vec<_> = validate(prog)
        .into_iter()
        .filter(|d| d.severity == Severity::Error && d.code == "T38")
        .map(|d| d.message)
        .collect();
    assert!(
        hits.iter().any(|m| m.contains(needle)),
        "expected T38 containing {needle:?}, got {hits:?}"
    );
}

#[test]
fn t38_rejects_non_scalar_literals_and_fuzz_void_result() {
    let prog = parse_pred(
        r#"
property "empty" for Counter {
    call getCount()
    expect {}
}
"#,
    );
    t38_message(&prog, "non-scalar literal");

    let prog = parse_pred(
        r#"
property "array" for Counter {
    call getCount()
    expect array(1)
}
"#,
    );
    t38_message(&prog, "non-scalar literal");

    let mut prog = parse_pred(
        r#"
property "void fuzz" (amount: u64) for Counter {
    call increment(amount)
    expect return > 0

    fuzz { amount in 0..10 }
}
"#,
    );
    desugar_properties(&mut prog);
    assert_eq!(prog.fuzz_tests.len(), 1, "range instance desugars to a fuzz");
    t38_message(&prog, "route 'increment' has no declared return type");

    let prog = parse_pred(
        r#"
test "void test" for Counter {
    call increment(1)
    expect return > 0
}
"#,
    );
    t38_message(&prog, "route 'increment' has no declared return type");
}

#[test]
fn t38_allows_constant_named_result_and_unused_result_param() {
    let src = r#"
entity Counter {
    routes {
        view getCount() -> u64 => [return(m_count)]
    }
    m_count: u64 {}
    const result: u64 = 1
}
property "const is not a clash" for Counter {
    call getCount()
    expect result > 0
}
"#;
    let mut prog = ProgramParser::new().parse(src).expect("must parse");
    ast::normalize_program_types(&mut prog);
    assert!(
        !error_codes(&prog).contains(&"T38".to_string()),
        "a constant named result is not a clash, got {:?}",
        error_codes(&prog)
    );

    let prog = parse_pred(
        r#"
property "unused result param" (result: u64) for Counter {
    call getCount()
    expect m_count >= 0
}
"#,
    );
    assert!(
        !error_codes(&prog).contains(&"T38".to_string()),
        "result is reserved only when the predicate mentions it, got {:?}",
        error_codes(&prog)
    );

    let mut prog = parse_pred(
        r#"
property "returns" (amount: u64) for Counter {
    call getCount()
    expect return >= amount

    fuzz { amount in 0..10 }
}
"#,
    );
    desugar_properties(&mut prog);
    assert!(
        error_codes(&prog).is_empty(),
        "fuzz predicate on a returning route must validate, got {:?}",
        error_codes(&prog)
    );
}

const PRED_SPEC: &str = r#"
property "holds" for Counter {
    call bump()
    call getCount()
    expect return != 0
    expect return > 0 && m_count >= 1
    expect m_count + 0 == m_count
}

property "eq stays eq" for Counter {
    call getCount()
    expect return 1
    expect state { m_count: 1 }
}

test "lens" for Counter {
    call pair()
    expect return.0 > 1
}
"#;

fn pred_suite() -> ast::Program {
    let src = r#"
entity Counter {
    routes {
        view getCount() -> u64 => [return(m_count)]
        view pair() -> (u64, u64) => [return((m_count, m_spent))]
        bump() => []
    }
    m_count: u64 { in bump() => m_count + 1 }
    m_spent: u64 { in bump() => m_spent }
}
"#;
    let mut prog = ProgramParser::new()
        .parse(&format!("{src}\n{PRED_SPEC}"))
        .expect("must parse");
    ast::normalize_program_types(&mut prog);
    prog
}

fn fn_body<'a>(src: &'a str, name: &str) -> &'a str {
    let marker = format!("function {name}()");
    let start = src
        .find(&marker)
        .unwrap_or_else(|| panic!("missing {marker} in:\n{src}"));
    let rest = &src[start..];
    let end = rest[marker.len()..]
        .find("\n    function ")
        .map(|i| marker.len() + i)
        .unwrap_or(rest.len());
    &rest[..end]
}

#[test]
fn expect_predicate_lowers_on_lean_and_foundry() {
    let mut prog = pred_suite();
    assert!(
        error_codes(&prog).is_empty(),
        "predicate fixture must validate, got {:?}",
        error_codes(&prog)
    );

    let spec = LeanBackend::default()
        .extra_files(&prog, "Counter")
        .into_iter()
        .find(|(path, _)| path.ends_with("CounterSpec.lean"))
        .map(|(_, src)| src)
        .expect("CounterSpec.lean");
    let holds = spec
        .split("namespace Counter.Spec.Properties")
        .nth(1)
        .expect("properties namespace");
    let holds_thm = holds
        .split("theorem ")
        .find(|s| s.contains("holds"))
        .expect("holds theorem");
    for needle in [
        "_result_0 !=",
        "_result_0 >",
        "&&",
        ".m_count",
        "≥",
        ".m_count +",
        "==",
    ] {
        assert!(
            holds_thm.contains(needle),
            "lean holds goal missing {needle:?}:\n{holds_thm}"
        );
    }
    let eq_thm = holds
        .split("theorem ")
        .find(|s| s.contains("eq_stays_eq") || s.contains("eq stays eq"))
        .expect("equality theorem");
    assert!(
        eq_thm.contains("_result_") && eq_thm.contains(" = "),
        "equality expect return stays an equality goal:\n{eq_thm}"
    );
    assert!(
        !eq_thm.contains("!="),
        "equality property must not become a predicate:\n{eq_thm}"
    );

    desugar_properties(&mut prog);
    let foundry = EvmSolidityBackend {
        deterministic_addresses: false,
    }
    .gen_test_files(&prog, "Counter");
    let sol = foundry
        .iter()
        .find(|(path, _)| path.ends_with(".t.sol"))
        .map(|(_, src)| src.as_str())
        .unwrap_or_else(|| panic!("no foundry test file: {:?}", foundry.iter().map(|(p, _)| p).collect::<Vec<_>>()));
    let holds_fn = fn_body(sol, "test_holds");
    for needle in [
        "uint64 _ret_1 = _counter.getCount();",
        "assertTrue((_ret_1 != 0), \"test 'holds': expect predicate failed\");",
        "assertTrue(((_ret_1 > 0) && (_counter.m_count() >= 1)), \"test 'holds': expect predicate failed\");",
        "assertTrue(((_counter.m_count() + 0) == _counter.m_count()), \"test 'holds': expect predicate failed\");",
    ] {
        assert!(holds_fn.contains(needle), "foundry holds missing {needle:?}:\n{holds_fn}");
    }
    let eq_fn = fn_body(sol, "test_eq_stays_eq");
    assert!(eq_fn.contains("assertEq"), "equality expect stays assertEq:\n{eq_fn}");
    assert!(!eq_fn.contains("assertTrue"), "equality expect must not use assertTrue:\n{eq_fn}");
    let lens_fn = fn_body(sol, "test_lens");
    assert!(
        lens_fn.contains("(uint64 _ret_1_0, uint64 _ret_1_1) = _counter.pair();"),
        "tuple return must bind slots:\n{lens_fn}"
    );
    assert!(
        lens_fn.contains("assertTrue((_ret_1_0 > 1), \"test 'lens': expect predicate failed\");"),
        "tuple lens must read the bound slot:\n{lens_fn}"
    );
}

#[cfg(feature = "revm")]
#[test]
fn expect_predicate_lowers_on_revm() {
    use cambrian_transpiler::codegen::evm_revm_test_codegen::generate_revm_tests;
    use cambrian_transpiler::project::{FuzzConfig, InvariantConfig, RevmTestConfig};

    let mut prog = pred_suite();
    desugar_properties(&mut prog);
    let files = generate_revm_tests(
        &prog,
        "counter",
        false,
        &RevmTestConfig {
            enabled: true,
            ..Default::default()
        },
        &FuzzConfig::default(),
        &InvariantConfig::default(),
    );
    let rust = files
        .iter()
        .find(|(p, _)| p == "revm-tests/tests/counter.rs")
        .map(|(_, src)| src.as_str())
        .expect("counter revm test");
    for needle in [
        "let _ret = t.call(ICounter::getCountCall {  });",
        "assert!((_ret != 0u64), \"test 'holds': expect predicate failed\");",
        "let m_count = { let _got = t.call(ICounter::m_countCall {}); _got };",
        "assert!(((_ret > 0u64) && (m_count >= 1u64)), \"test 'holds': expect predicate failed\");",
        "assert!(((m_count + 0u64) == m_count), \"test 'holds': expect predicate failed\");",
        "assert!((_ret.0 > 1u64), \"test 'lens': expect predicate failed\");",
    ] {
        assert!(rust.contains(needle), "revm holds missing {needle:?}:\n{rust}");
    }
    let eq = rust
        .split("fn test_eq_stays_eq")
        .nth(1)
        .expect("eq test");
    assert!(eq.contains("assert_eq!"), "equality expect stays assert_eq:\n{eq}");
    let eq_head = eq.split("fn test_").next().unwrap_or(eq);
    assert!(
        !eq_head.contains("expect predicate failed"),
        "equality expect must not emit a predicate assert:\n{eq_head}"
    );
}

#[cfg(feature = "rust-targets")]
#[test]
fn expect_predicate_lowers_on_native() {
    use cambrian_transpiler::codegen::test_codegen::generate_tests;
    use cambrian_transpiler::project::{FuzzConfig, InvariantConfig};

    let mut prog = pred_suite();
    desugar_properties(&mut prog);
    let rust = generate_tests(
        &prog,
        "counter_tests",
        &FuzzConfig::default(),
        &InvariantConfig::default(),
    )
    .expect("native tests");
    for needle in [
        "let _ret_val_2 = <u64>::de_be(",
        "assert!((_ret_val_2 != 0), \"test 'holds': expect predicate failed\");",
        "assert!(((_ret_val_2 > 0) && (_new_state_2.m_count >= 1)), \"test 'holds': expect predicate failed\");",
        "assert!(((_new_state_2.m_count + 0) == _new_state_2.m_count), \"test 'holds': expect predicate failed\");",
        "assert!((_ret_val.clone().0 > 1), \"test 'lens': expect predicate failed\");",
    ] {
        assert!(rust.contains(needle), "native holds missing {needle:?}:\n{rust}");
    }
}
