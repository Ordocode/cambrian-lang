// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Diagnostics-only parse recovery. Never feeds a recovered tree into
//! validation or codegen.

use crate::diagnostic::diagnostic_from_parse_error;
use crate::validate::Diagnostic;
use crate::ProgramParser;

const TOPLEVEL_KWS: &[&str] = &[
    "entity", "pure", "test", "fuzz", "property", "invariant", "record", "enum", "type",
    "event", "error", "library", "extern", "import", "use", "const", "macro",
];

/// Collect parse diagnostics from `src` without returning an AST.
/// The first attempt is a strict parse; on failure, skip to the next
/// synchronization point and retry until `--max-errors` or EOF.
pub fn recover_parse_diagnostics(
    src: &str,
    path: &str,
    max_errors: usize,
) -> Vec<Diagnostic> {
    let max_errors = max_errors.max(1).min(10_000);
    let mut diags = Vec::new();
    let mut pos = 0usize;
    let mut guard = 0usize;
    loop {
        if diags.len() >= max_errors || pos >= src.len() || guard > src.len() + 2 {
            break;
        }
        guard += 1;
        let slice = &src[pos..];
        match ProgramParser::new().parse(slice) {
            Ok(_) if pos == 0 => return Vec::new(),
            Ok(_) => break,
            Err(err) => {
                let d = diagnostic_from_parse_error(&err, path, src, pos);
                let start = d.span.as_ref().map(|s| s.byte_start).unwrap_or(pos);
                let end = d.span.as_ref().map(|s| s.byte_end).unwrap_or(pos + 1);
                if pos == 0 {
                    diags.push(d);
                } else if !diags.iter().any(|prev| prev.span == d.span && prev.code == d.code) {
                    diags.push(d);
                } else {
                    pos = end.saturating_add(1).max(pos + 1);
                    continue;
                }
                let from_kw = src.get(start..).and_then(|rest| {
                    let (ident, _) = read_ident(rest, 0);
                    if TOPLEVEL_KWS.contains(&ident) && start > pos {
                        Some(start)
                    } else {
                        None
                    }
                });
                if let Some(kw) = from_kw {
                    pos = kw;
                    continue;
                }
                match next_sync_point(src, end.max(pos + 1)) {
                    Some(n) if n > pos => pos = n,
                    _ => {
                        pos = end.saturating_add(1).max(pos + 1);
                        if pos >= src.len() {
                            break;
                        }
                    }
                }
            }
        }
    }
    diags
}

/// Byte offset of the next recovery sync: after `;` / `]` / `}`, or at the
/// next top-level declaration keyword (brace depth 0).
fn next_sync_point(src: &str, from: usize) -> Option<usize> {
    let bytes = src.as_bytes();
    if from >= bytes.len() {
        return None;
    }
    let mut i = from;
    let mut depth_brace: i32 = depth_before(src, from);
    let mut in_str = false;
    let mut in_line_comment = false;
    while i < bytes.len() {
        let c = bytes[i];
        if in_line_comment {
            if c == b'\n' {
                in_line_comment = false;
            }
            i += 1;
            continue;
        }
        if in_str {
            if c == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if c == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if c == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            in_line_comment = true;
            i += 2;
            continue;
        }
        if c == b'"' {
            in_str = true;
            i += 1;
            continue;
        }
        match c {
            b'{' => depth_brace += 1,
            b'}' => {
                depth_brace = depth_brace.saturating_sub(1);
                i += 1;
                if depth_brace == 0 {
                    return Some(skip_ws(src, i));
                }
                continue;
            }
            b']' | b';' => {
                i += 1;
                return Some(i);
            }
            _ => {}
        }
        if depth_brace == 0 && is_ident_start(c) {
            let (ident, end) = read_ident(src, i);
            if TOPLEVEL_KWS.contains(&ident) && end > from {
                return Some(i);
            }
            i = end;
            continue;
        }
        i += 1;
    }
    None
}

fn depth_before(src: &str, until: usize) -> i32 {
    let bytes = src.as_bytes();
    let mut depth = 0i32;
    let mut i = 0;
    let mut in_str = false;
    let mut in_line_comment = false;
    let until = until.min(bytes.len());
    while i < until {
        let c = bytes[i];
        if in_line_comment {
            if c == b'\n' {
                in_line_comment = false;
            }
            i += 1;
            continue;
        }
        if in_str {
            if c == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if c == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if c == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            in_line_comment = true;
            i += 2;
            continue;
        }
        if c == b'"' {
            in_str = true;
            i += 1;
            continue;
        }
        if c == b'{' {
            depth += 1;
        } else if c == b'}' {
            depth = depth.saturating_sub(1);
        }
        i += 1;
    }
    depth
}

fn skip_ws(src: &str, mut i: usize) -> usize {
    let bytes = src.as_bytes();
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

fn is_ident_start(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphabetic()
}

fn is_ident_continue(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphanumeric()
}

fn read_ident(src: &str, start: usize) -> (&str, usize) {
    let bytes = src.as_bytes();
    if start >= bytes.len() || !is_ident_start(bytes[start]) {
        return ("", start);
    }
    let mut i = start + 1;
    while i < bytes.len() && is_ident_continue(bytes[i]) {
        i += 1;
    }
    (&src[start..i], i)
}
