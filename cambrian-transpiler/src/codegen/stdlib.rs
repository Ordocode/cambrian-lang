// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Standard library (`std::`) lowering helpers — see `docs/STDLIB.md`.

use crate::ast::{Expr, Type};

use super::std_str::{
    is_std_str_parse_call, parse_str_meta, parse_str_return_type, ParseStrMeta,
};

/// `namespace` is the combined module path (`std::math`, `std::str`, …).
pub fn is_std_namespace(namespace: &str) -> bool {
    namespace.starts_with("std::")
}

pub fn is_supported_std_call(namespace: &str, name: &str) -> bool {
    match namespace {
        "std::math" => matches!(
            name,
            "min" | "max" | "abs" | "clamp" | "muldiv" | "muldivmod"
                | "divmod" | "divc" | "divr" | "sign" | "minmax"
                | "modpow2" | "pow"
        ),
        "std::str" => parse_str_meta(name).is_some() || name == "format",
        "std::crypto" => name == "sha256",
        _ => false,
    }
}

pub fn infer_std_call_return_type(namespace: &str, name: &str) -> Option<Type> {
    if namespace == "std::str" {
        if let Some(meta) = parse_str_meta(name) {
            return Some(parse_str_return_type(&meta));
        }
        if name == "format" {
            return Some(Type::Simple("String".to_string()));
        }
    }
    match (namespace, name) {
        ("std::crypto", "sha256") => Some(Type::Generic(
            "Vec".to_string(),
            vec![Type::Simple("u8".to_string())],
        )),
        ("std::math", "divmod") | ("std::math", "minmax") => Some(Type::Tuple(vec![
            Type::Simple("u64".to_string()),
            Type::Simple("u64".to_string()),
        ])),
        ("std::math", _) => Some(Type::Simple("u64".to_string())),
        _ => None,
    }
}

pub fn is_std_string_to_numeric(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::NamespacedCall {
            namespace,
            name,
            ..
        } if is_std_str_parse_call(namespace, name)
    )
}

pub fn is_std_numeric_to_string(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::NamespacedCall {
            namespace,
            name,
            ..
        } if namespace == "std::str" && name == "format"
    )
}

pub fn is_numeric_type(ty: &Type) -> bool {
    match ty {
        Type::Simple(name) => matches!(
            name.as_str(),
            "u8" | "u16" | "u32" | "u64" | "u128" | "U256"
                | "i8" | "i16" | "i32" | "i64" | "i128"
        ),
        _ => false,
    }
}

pub fn is_string_type(ty: &Type) -> bool {
    matches!(ty, Type::Simple(name) if name == "String")
}

/// Solidity call returning `(bool ok, uint256 value)` or signed variant.
pub fn gen_std_try_parse_evm(meta: &ParseStrMeta, args: &[String]) -> String {
    let s = &args[0];
    let radix = args.get(1).map(|s| s.as_str()).unwrap_or("10");
    if meta.signed {
        format!("_cam_try_parse_radix_signed({}, {}, {})", s, radix, meta.bits)
    } else {
        format!("_cam_try_parse_radix({}, {}, {})", s, radix, meta.bits)
    }
}

fn gen_std_parse_rust(meta: &ParseStrMeta, args: &[String]) -> String {
    let s = &args[0];
    let radix = args.get(1).map(|s| s.as_str()).unwrap_or("10");
    if !meta.signed && meta.bits >= 256 {
        return format!(
            "{{ cam_try_parse_radix_u256(&{}, {} as u32) }}",
            s, radix
        );
    }
    let inner = super::std_str::parse_str_inner_type(meta);
    let rust_ty = match &inner {
        Type::Simple(n) => n.as_str(),
        _ => "u64",
    };
    if meta.signed {
        format!(
            "{{ let __radix = {} as u32; match {}::from_str_radix(&{}, __radix) {{ Ok(v) => Option::Some(v), Err(_) => Option::None }} }}",
            radix, rust_ty, s
        )
    } else {
        format!(
            "{{ let __radix = {} as u32; match u128::from_str_radix(&{}, __radix) {{ Ok(v) => match {}::try_from(v) {{ Ok(x) => Option::Some(x), Err(_) => Option::None }}, Err(_) => Option::None }} }}",
            radix, s, rust_ty
        )
    }
}

/// Erased Option value (`0` = none) for inline Solidity expressions.
pub fn gen_std_parse_evm_erased(meta: &ParseStrMeta, args: &[String]) -> String {
    let s = &args[0];
    let radix = args.get(1).map(|s| s.as_str()).unwrap_or("10");
    if meta.signed {
        format!("_cam_parse_radix_signed_or_zero({}, {}, {})", s, radix, meta.bits)
    } else {
        format!("_cam_parse_radix_or_zero({}, {}, {})", s, radix, meta.bits)
    }
}

/// Container / Acki Nacki Rust lowering for `std::module::fn(...)`.
pub fn gen_std_call_rust(namespace: &str, name: &str, args: &[String]) -> String {
    if namespace == "std::str" {
        if let Some(meta) = parse_str_meta(name) {
            return gen_std_parse_rust(&meta, args);
        }
    }
    match (namespace, name) {
        ("std::math", "min") => format!(
            "if {} < {} {{ {} }} else {{ {} }}",
            args[0], args[1], args[0], args[1]
        ),
        ("std::math", "max") => format!(
            "if {} > {} {{ {} }} else {{ {} }}",
            args[0], args[1], args[0], args[1]
        ),
        ("std::math", "abs") => format!("abs({})", args[0]),
        ("std::math", "clamp") => format!("clamp({}, {}, {})", args[0], args[1], args[2]),
        ("std::math", "muldiv") => format!("cam_muldiv({}, {}, {})", args[0], args[1], args[2]),
        ("std::math", "muldivmod") => format!("cam_muldivmod({}, {}, {})", args[0], args[1], args[2]),
        ("std::math", "divmod") => format!("cam_divmod({}, {})", args[0], args[1]),
        ("std::math", "divc") => format!("cam_divc({}, {})", args[0], args[1]),
        ("std::math", "divr") => format!("cam_divr({}, {})", args[0], args[1]),
        ("std::math", "sign") => format!("cam_sign({})", args[0]),
        ("std::math", "minmax") => format!("cam_minmax({}, {})", args[0], args[1]),
        ("std::math", "modpow2") => format!("cam_modpow2({}, {})", args[0], args[1]),
        ("std::math", "pow") => format!("cam_pow({}, {})", args[0], args[1]),
        ("std::str", "format") => gen_std_format_rust(args),
        ("std::crypto", "sha256") => format!("cam_sha256(&{})", args.join(", ")),
        _ => format!("compile_error!(\"unsupported standard-library call {}::{}\")",
            namespace, name),
    }
}

fn gen_std_format_rust(args: &[String]) -> String {
    if args.is_empty() {
        return "String::new()".to_string();
    }
    if args.len() == 2 {
        let fmt = &args[0];
        let val = &args[1];
        if fmt.contains("{:06}") {
            return format!(
                "{{ let __t = ({val}).to_string(); let __pad = 6usize.saturating_sub(__t.len()); format!(\"{{}}{{}}\", \"0\".repeat(__pad), __t) }}",
                val = val
            );
        }
        if fmt.contains("{}") {
            return format!("({}).to_string()", val);
        }
    }
    format!(
        "std_format({}{})",
        args[0],
        args.iter().skip(1).map(|a| format!(", {}", a)).collect::<String>()
    )
}

const CAM_TRY_PARSE_U256_RUST: &str = r#"
fn cam_char_radix_digit(c: char, base: u32) -> Option<u32> {
    if c.is_ascii_digit() {
        let d = c as u32 - 48;
        if d < base { Some(d) } else { None }
    } else if c.is_ascii_uppercase() {
        let d = c as u32 - 55;
        if d < base { Some(d) } else { None }
    } else if c.is_ascii_lowercase() {
        let d = c as u32 - 87;
        if d < base { Some(d) } else { None }
    } else {
        None
    }
}

fn cam_try_parse_radix_u256(s: &str, radix: u32) -> Option<U256> {
    let mut base = radix;
    let rest = if s.len() >= 2 && s.starts_with("0x") || s.starts_with("0X") {
        if base != 0 && base != 16 { return None; }
        base = 16;
        &s[2..]
    } else if base == 0 {
        base = 10;
        s
    } else {
        s
    };
    if base < 2 || base > 36 || rest.is_empty() {
        return None;
    }
    let base_u = U256::from_u64(base as u64);
    let mut n = U256::ZERO;
    for c in rest.chars() {
        let digit = cam_char_radix_digit(c, base)?;
        let digit_u = U256::from_u64(digit as u64);
        if n != U256::ZERO && n > U256::MAX / base_u {
            return None;
        }
        let scaled = n * base_u;
        if scaled > U256::MAX - digit_u {
            return None;
        }
        n = scaled + digit_u;
    }
    Some(n)
}
"#;

pub fn gen_container_std_rust_helpers(program: &crate::ast::Program) -> String {
    let mut out = String::new();
    if super::std_str::std_str_u256_parse_used_in_program(program) {
        out.push_str(CAM_TRY_PARSE_U256_RUST);
    }
    out
}

/// Solidity expression for `std::module::fn(...)`.
pub fn gen_std_call_evm(namespace: &str, name: &str, args: &[String]) -> String {
    if namespace == "std::str" {
        if let Some(meta) = parse_str_meta(name) {
            return gen_std_parse_evm_erased(&meta, args);
        }
    }
    match (namespace, name) {
        ("std::math", "min") => format!("_cam_std_min({}, {})", args[0], args[1]),
        ("std::math", "max") => format!("_cam_std_max({}, {})", args[0], args[1]),
        ("std::math", "clamp") => format!("_cam_std_clamp({}, {}, {})", args[0], args[1], args[2]),
        ("std::math", "muldiv") => format!("_cam_muldiv({}, {}, {})", args[0], args[1], args[2]),
        ("std::math", "divc") => format!("_cam_std_divc({}, {})", args[0], args[1]),
        ("std::math", "divr") => format!("_cam_std_divr({}, {})", args[0], args[1]),
        ("std::math", "divmod") => format!("_cam_std_divmod({}, {})", args[0], args[1]),
        ("std::math", "abs") => format!("_cam_abs({})", args[0]),
        ("std::math", "sign") => format!("_cam_sign({})", args[0]),
        ("std::math", "minmax") => format!(
            "(_cam_std_min({}, {}), _cam_std_max({}, {}))",
            args[0], args[1], args[0], args[1]
        ),
        ("std::math", "muldivmod") => format!("_cam_muldivmod({}, {}, {})", args[0], args[1], args[2]),
        ("std::math", "pow") => format!("_cam_pow({}, {})", args[0], args[1]),
        ("std::math", "modpow2") => format!("_cam_modpow2({}, {})", args[0], args[1]),
        ("std::str", "format") => gen_std_format_evm(args),
        ("std::crypto", "sha256") => format!("abi.encodePacked(sha256(bytes({})))", args[0]),
        _ => format!("(/* std::{}/{} unsupported */ 0)", namespace, name),
    }
}

fn gen_std_format_evm(args: &[String]) -> String {
    if args.len() == 2 && args[0] == "\"{:06}\"" {
        return format!("_cam_format_pad6({}, {})", args[1], "6");
    }
    if args.len() == 2 && args[0] == "\"{}\"" {
        return format!("_cam_itoa(int256({}))", args[1]);
    }
    if args.len() == 2 {
        if let Some(inner) = args[0]
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
        {
            if let Some(prefix) = inner.strip_suffix("{}") {
                if !prefix.contains("{}") {
                    let escaped = prefix.replace('\\', "\\\\").replace('"', "\\\"");
                    return format!(
                        "string(abi.encodePacked(\"{}\", _cam_itoa(int256(uint256({})))))",
                        escaped,
                        args[1]
                    );
                }
            }
        }
    }
    format!(
        "_cam_format({}{})",
        args[0],
        args.iter().skip(1).map(|a| format!(", {}", a)).collect::<String>()
    )
}

/// Whether a Solidity helper emitted by [`gen_std_call_evm`] is needed in the project.
#[allow(dead_code)]
pub fn std_evm_helper_needed(namespace: &str, name: &str) -> Option<&'static str> {
    match (namespace, name) {
        ("std::math", "pow") => Some("pow"),
        ("std::math", "modpow2") => Some("modpow2"),
        ("std::math", "abs") => Some("abs"),
        ("std::math", "sign") => Some("sign"),
        ("std::math", "muldivmod") => Some("muldivmod"),
        ("std::str", "format") => Some("format"),
        _ => None,
    }
}
