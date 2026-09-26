// Copyright (C) 2025-2026 The Cambrian Authors
// SPDX-License-Identifier: GPL-3.0-only

use serde::Serialize;

/// Converts byte offsets (from AST spans) to line numbers.
pub struct SourceMapper {
    line_starts: Vec<usize>,
}

impl SourceMapper {
    pub fn new(source: &str) -> Self {
        let mut line_starts = vec![0];
        for (i, ch) in source.char_indices() {
            if ch == '\n' {
                line_starts.push(i + 1);
            }
        }
        SourceMapper { line_starts }
    }

    /// 1-based column number (byte offset from line start + 1).
    pub fn column_of(&self, offset: usize) -> usize {
        let line = self.line_of(offset);
        let start = self.line_starts.get(line.saturating_sub(1)).copied().unwrap_or(0);
        offset.saturating_sub(start) + 1
    }

    /// 1-based line number for a byte offset.
    pub fn line_of(&self, offset: usize) -> usize {
        match self.line_starts.binary_search(&offset) {
            Ok(idx) => idx + 1,
            Err(idx) => idx,
        }
    }
}

/// One entry in a source-map JSON file.
#[derive(Debug, Serialize)]
pub struct SourceMapEntry {
    pub gen_line: usize,
    pub cam_line: usize,
    pub kind: &'static str,
    pub name: String,
}

/// Accumulates entries during codegen.
#[derive(Default)]
pub struct SourceMapBuilder {
    entries: Vec<SourceMapEntry>,
}

impl SourceMapBuilder {
    pub fn new() -> Self { Self::default() }

    pub fn add(&mut self, gen_line: usize, cam_line: usize, kind: &'static str, name: String) {
        self.entries.push(SourceMapEntry { gen_line, cam_line, kind, name });
    }

    pub fn into_entries(self) -> Vec<SourceMapEntry> { self.entries }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(&self.entries).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_of_single_line() {
        let sm = SourceMapper::new("hello world");
        assert_eq!(sm.line_of(0), 1);
        assert_eq!(sm.line_of(5), 1);
    }

    #[test]
    fn line_of_multi_line() {
        let sm = SourceMapper::new("aaa\nbbb\nccc\n");
        assert_eq!(sm.line_of(0), 1);  // 'a'
        assert_eq!(sm.line_of(3), 1);  // '\n'
        assert_eq!(sm.line_of(4), 2);  // 'b'
        assert_eq!(sm.line_of(8), 3);  // 'c'
    }

    #[test]
    fn line_of_empty() {
        let sm = SourceMapper::new("");
        assert_eq!(sm.line_of(0), 1);
    }

    #[test]
    fn column_of_first_line() {
        let sm = SourceMapper::new("hello world");
        assert_eq!(sm.column_of(0), 1);
        assert_eq!(sm.column_of(6), 7);
    }
}
