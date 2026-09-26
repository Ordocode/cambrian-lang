// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! Typed diagnostic protocol (human + JSON) shared by parse, project-load,
//! validation, and target-compat.

use crate::sourcemap::SourceMapper;
use crate::validate::{Diagnostic, Phase, Severity, SourceSpan};
use lalrpop_util::ParseError;
use serde_json::{json, Value};

pub const DIAGNOSTICS_SCHEMA: &str = "cambrian.diagnostics/v1";
pub const BUILD_IDENTITY_SCHEMA: &str = "cambrian.build-identity/v1";
pub const DEFAULT_MAX_ERRORS: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagFormat {
    Human,
    Json,
}

pub fn span_from_source(source: &str, byte_start: usize, byte_end: usize) -> SourceSpan {
    let mapper = SourceMapper::new(source);
    let line = mapper.line_of(byte_start);
    let column = mapper.column_of(byte_start);
    SourceSpan {
        byte_start,
        byte_end: byte_end.max(byte_start),
        line,
        column,
    }
}

pub fn diagnostic_from_parse_error<T, E>(
    err: &ParseError<usize, T, E>,
    path: &str,
    source: &str,
    base: usize,
) -> Diagnostic
where
    T: std::fmt::Display,
    E: std::fmt::Display,
{
    let mut d = match err {
        ParseError::InvalidToken { location } => {
            let loc = location + base;
            let mut d = Diagnostic::error("PARSE_INVALID_TOKEN", format!("Invalid token at byte {loc}"));
            d.span = Some(span_from_source(source, loc, loc + 1));
            d
        }
        ParseError::UnrecognizedEof { location, expected } => {
            let loc = location + base;
            let mut d = Diagnostic::error(
                "PARSE_EOF",
                format!("Unexpected end of file at byte {loc}"),
            );
            d.span = Some(span_from_source(source, loc, loc));
            d.expected = Some(expected.clone());
            d
        }
        ParseError::UnrecognizedToken {
            token: (start, tok, end),
            expected,
        } => {
            let start = start + base;
            let end = end + base;
            let unexpected = tok.to_string();
            let mut d = Diagnostic::error(
                "PARSE_UNEXPECTED_TOKEN",
                format!("Unexpected token `{unexpected}`"),
            );
            d.span = Some(span_from_source(source, start, end));
            d.unexpected = Some(unexpected);
            d.expected = Some(
                expected
                    .iter()
                    .filter(|t| !TVM_ONLY_TOKENS.contains(&t.as_str()))
                    .cloned()
                    .collect(),
            );
            d
        }
        ParseError::ExtraToken { token: (start, tok, end) } => {
            let start = start + base;
            let end = end + base;
            let unexpected = tok.to_string();
            let mut d = Diagnostic::error(
                "PARSE_EXTRA_TOKEN",
                format!("Extra token `{unexpected}`"),
            );
            d.span = Some(span_from_source(source, start, end));
            d.unexpected = Some(unexpected);
            d
        }
        ParseError::User { error } => {
            Diagnostic::error("PARSE_USER", format!("{error}"))
        }
    };
    d.phase = Phase::Parse;
    d.path = Some(path.to_string());
    d
}

/// Acki Nacki bounce-handling keywords. They still parse, but are not offered
/// as suggestions because every other target rejects them (E26).
const TVM_ONLY_TOKENS: &[&str] = &["\"rescue\"", "\"recover\"", "\"onBounce\""];

pub fn format_human(d: &Diagnostic) -> String {
    let sev = if d.downgraded {
        "note"
    } else {
        match d.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    };
    let loc = match (&d.path, &d.span) {
        (Some(path), Some(span)) => format!("{path}:{}:{}: ", span.line, span.column),
        (Some(path), None) => format!("{path}: "),
        (None, Some(span)) => format!("{}:{}: ", span.line, span.column),
        (None, None) => String::new(),
    };
    let mut line = format!("{loc}{sev} [{}]: {}", d.code, d.message);
    if let Some(hint) = &d.hint {
        line.push_str(&format!("\n  hint: {hint}"));
    }
    if let Some(expected) = &d.expected {
        if !expected.is_empty() {
            line.push_str(&format!("\n  expected one of: {}", expected.join(", ")));
        }
    }
    line
}

pub fn diagnostic_to_json(d: &Diagnostic) -> Value {
    let severity = if d.downgraded {
        "note"
    } else {
        match d.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    };
    let phase = match d.phase {
        Phase::Parse => "parse",
        Phase::Project => "project",
        Phase::Validate => "validate",
        Phase::Target => "target",
    };
    let mut obj = json!({
        "code": d.code,
        "severity": severity,
        "phase": phase,
        "message": d.message,
    });
    if let Some(path) = &d.path {
        obj["path"] = json!(path);
    }
    if let Some(span) = &d.span {
        obj["span"] = json!({
            "byteStart": span.byte_start,
            "byteEnd": span.byte_end,
            "line": span.line,
            "column": span.column,
        });
    }
    if let Some(hint) = &d.hint {
        obj["hint"] = json!(hint);
    }
    if let Some(u) = &d.unexpected {
        obj["unexpected"] = json!(u);
    }
    if let Some(exp) = &d.expected {
        obj["expected"] = json!(exp);
    }
    if let Some(sup) = &d.suppressed_by {
        obj["suppressedBy"] = json!(sup);
    }
    if d.downgraded {
        obj["downgraded"] = json!(true);
    }
    obj
}

pub fn build_identity_json() -> Value {
    json!({
        "schema": BUILD_IDENTITY_SCHEMA,
        "version": env!("CARGO_PKG_VERSION"),
        "gitCommit": option_env!("CAMBRIAN_GIT_COMMIT").unwrap_or("unknown"),
        "dirty": option_env!("CAMBRIAN_GIT_DIRTY").unwrap_or("true") == "true",
        "buildProfile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "grammarRevision": option_env!("CAMBRIAN_GRAMMAR_REVISION").unwrap_or("unknown"),
        "diagnosticProtocol": DIAGNOSTICS_SCHEMA,
    })
}

pub fn diagnostics_envelope(diagnostics: &[Diagnostic]) -> Value {
    json!({
        "schema": DIAGNOSTICS_SCHEMA,
        "compiler": build_identity_json(),
        "diagnostics": diagnostics.iter().map(diagnostic_to_json).collect::<Vec<_>>(),
    })
}

pub fn print_diagnostics(diagnostics: &[Diagnostic], format: DiagFormat, include_suppressed: bool) {
    let visible: Vec<&Diagnostic> = diagnostics
        .iter()
        .filter(|d| include_suppressed || d.is_actionable())
        .collect();
    match format {
        DiagFormat::Human => {
            for d in visible {
                eprintln!("{}", format_human(d));
            }
        }
        DiagFormat::Json => {
            let owned: Vec<Diagnostic> = visible.into_iter().cloned().collect();
            println!("{}", diagnostics_envelope(&owned));
        }
    }
}

/// JSON keeps suppressed secondaries; human hides them.
pub fn print_diagnostics_default(diagnostics: &[Diagnostic], format: DiagFormat) {
    match format {
        DiagFormat::Human => print_diagnostics(diagnostics, format, false),
        DiagFormat::Json => print_diagnostics(diagnostics, format, true),
    }
}

pub fn has_errors(diagnostics: &[Diagnostic]) -> bool {
    diagnostics.iter().any(|d| {
        matches!(d.severity, Severity::Error) && !d.downgraded
    })
}
