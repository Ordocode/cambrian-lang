// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

//! E27 — the build boundary that refuses a substituted value.
//!
//! Expression codegen has two arms that cannot lower their input and fall back
//! to the literal `0`: an unknown namespaced call, and the catch-all `_` arm.
//! Both are defence in depth behind validator E15/E16 — but E15/E16 enumerate
//! *known-bad* `Expr` shapes, while the codegen fallback catches *everything
//! unhandled*. The two sets drift apart by construction, and every shape in the
//! gap becomes a `0` with a comment above it.
//!
//! That is a silent miscompile: `pure fn wide(a) { (0..8).fold((0, a), ..).0 }`
//! transpiled to `function wide(uint256 a) pure returns (uint256) { return 0; }`
//! and the CLI exited 0. The comment named the validator that should have
//! caught it, the artifact went to disk, `forge test` was green on paths that
//! never reached the branch, and a security review found it downstream.
//!
//! So the marker is scanned at the write boundary and fails the transpile. A
//! shape the validator forgot still stops the build; it just stops it here.

/// Tail token on the two expression-codegen comments that accompany a
/// substituted `0`. Appended rather than prefixed so the existing audit-matrix
/// probes, which match on the leading text, keep matching.
pub const UNLOWERED_VALUE_MARKER: &str = "[unlowered-value]";

/// A generated line where codegen substituted `0` for an expression it could
/// not lower.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnloweredValue {
    /// Path of the generated file, as the backend named it.
    pub file: String,
    /// 1-based line number within that file.
    pub line: usize,
    /// The sentinel comment, trimmed.
    pub comment: String,
}

/// Scan generated Solidity for substituted values.
///
/// Takes the backend's `(relative path, contents)` pairs. Non-Solidity outputs
/// are skipped: the marker is emitted by Solidity expression codegen, and a
/// `.cam` fixture or README quoting it is not a miscompile.
pub fn find_unlowered_values(files: &[(String, String)]) -> Vec<UnloweredValue> {
    let mut found = Vec::new();
    for (path, contents) in files {
        if !path.ends_with(".sol") {
            continue;
        }
        for (index, line) in contents.lines().enumerate() {
            if line.contains(UNLOWERED_VALUE_MARKER) {
                found.push(UnloweredValue {
                    file: path.clone(),
                    line: index + 1,
                    comment: line.trim().to_string(),
                });
            }
        }
    }
    found
}

/// Operator-facing message for a non-empty [`find_unlowered_values`] result.
pub fn unlowered_value_report(found: &[UnloweredValue]) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "error [E27]: {} expression(s) had no EVM lowering and were replaced by the literal `0`\n",
        found.len()
    ));
    for entry in found {
        out.push_str(&format!("  {}:{}: {}\n", entry.file, entry.line, entry.comment));
    }
    out.push_str(
        "\nThe generated Solidity would compile and run, returning 0 where a value was expected.\n\
         Either lower the expression for the EVM target, or reject it in the validator\n\
         (`check_evm_compat_expr`, E16) so the failure names the source construct.\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_marker_with_its_line() {
        let files = vec![(
            "src/p.sol".to_string(),
            format!("function w() pure returns (uint256) {{\n    // nope {}\n    return 0;\n}}\n", UNLOWERED_VALUE_MARKER),
        )];
        let found = find_unlowered_values(&files);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].line, 2);
        assert_eq!(found[0].file, "src/p.sol");
    }

    #[test]
    fn ignores_clean_output_and_non_solidity_files() {
        let files = vec![
            ("src/p.sol".to_string(), "function w() pure returns (uint256) { return 1; }\n".to_string()),
            ("README.md".to_string(), format!("we emit {} on failure\n", UNLOWERED_VALUE_MARKER)),
        ];
        assert!(find_unlowered_values(&files).is_empty());
    }
}
