//! Whether a copied text is code (source, config, SQL, a shell session): the
//! window shows such clips in a monospaced font. Decided once, when the clip
//! is saved, from its preview (at most `store::PREVIEW_CHARS` characters).
//!
//! Line by line, counting signs that prose rarely has: a line ending in `;`
//! or `{`, a keyword at the start (`fn`, `def`, `SELECT`…), operators (`=>`,
//! `==`, `::`), a call `name(`, a `"key":` pair, a tag. One sign may be an
//! accident, so a single line needs two; a longer text needs signs in at
//! least half of its lines (indented lines count too) and two in all.

/// Lines looked at: enough to decide, cheap on a long text.
const MAX_LINES: usize = 60;

/// First words that start a line of code. Case matters: prose starts
/// sentences with "If", "For", "Import".
const KEYWORDS: &[&str] = &[
    "fn", "pub", "let", "mut", "const", "var", "function", "def", "class", "import", "export", "return", "package",
    "use", "impl", "struct", "enum", "interface", "trait", "async", "await", "if", "elif", "else", "for", "while",
    "switch", "case", "try", "catch", "except", "finally", "public", "private", "protected", "static", "void",
    "#include", "#define", "#import", "#!", "//", "/*", "*/", "<?php", ">>>", "$", "SELECT", "INSERT", "UPDATE",
    "DELETE", "CREATE", "ALTER", "DROP", "FROM", "WHERE",
];

/// Line endings of code.
const ENDINGS: &[&str] = &[";", "{", "}", "[", "(", "=>", "\\", "*/"];

/// Operators and pairs seen in code, not in prose.
const OPERATORS: &[&str] = &[
    "=>", "->", "::", "==", "!=", "&&", "||", "+=", "-=", ":=", "();", "){", ") {", "</", "/>", " = ",
];

pub fn looks_like_code(text: &str) -> bool {
    let mut lines = 0;
    let mut code_lines = 0;
    let mut signs = 0;
    for line in text.lines().filter(|l| !l.trim().is_empty()).take(MAX_LINES) {
        lines += 1;
        let (line_signs, indented) = signs_in(line);
        signs += line_signs;
        if line_signs > 0 || indented {
            code_lines += 1;
        }
    }
    match lines {
        0 => false,
        1 => signs >= 2,
        _ => code_lines * 2 >= lines && signs >= 2,
    }
}

/// Signs of code in one line, and whether it is indented.
fn signs_in(line: &str) -> (u32, bool) {
    let t = line.trim();
    // list items ("- milk;", "* a", "1. b") are prose even with code-like ends
    if is_list_item(t) {
        return (0, false);
    }
    let mut signs = 0;
    let first = t.split_whitespace().next().unwrap_or("");
    let first_word = first.split(['(', '{', ':']).next().unwrap_or("");
    // symbols ("//x", "#!/bin/sh") may run into the next word
    let symbol_start = |k: &&str| !k.starts_with(|c: char| c.is_alphanumeric()) && t.starts_with(*k);
    if KEYWORDS.contains(&first) || KEYWORDS.contains(&first_word) || KEYWORDS.iter().any(symbol_start) {
        signs += 1;
    }
    if ENDINGS.iter().any(|e| t.ends_with(e)) {
        signs += 1;
    }
    signs += OPERATORS.iter().filter(|o| t.contains(*o)).count().min(2) as u32;
    if has_call(t) {
        signs += 1;
    }
    // "key": value (JSON), <tag …> (HTML, XML)
    if (t.starts_with('"') && t.contains("\":")) || (t.starts_with('<') && t.contains('>')) {
        signs += 1;
    }
    // a sentence: words, ending like one
    if t.ends_with(['.', '!', '?', '…']) && t.split_whitespace().count() >= 4 {
        signs = signs.saturating_sub(1);
    }
    let indented = line.starts_with('\t') || line.starts_with("  ");
    (signs, indented)
}

fn is_list_item(t: &str) -> bool {
    if t.starts_with("- ") || t.starts_with("* ") || t.starts_with("• ") {
        return true;
    }
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && (t[digits..].starts_with(". ") || t[digits..].starts_with(") "))
}

/// `name(` — a call or a definition; prose puts a space before a bracket.
fn has_call(t: &str) -> bool {
    t.match_indices('(').any(|(i, _)| {
        t[..i]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_is_detected() {
        for s in [
            "fn main() {}",
            "const x = 5;",
            "SELECT * FROM clips WHERE id = 1;",
            "let total = items.iter().map(|i| i.price).sum::<u32>();",
            "def add(a, b):\n    return a + b\n",
            "{\n  \"name\": \"pastezo\",\n  \"version\": 1\n}",
            ".row {\n  color: red;\n  padding: 4px;\n}",
            "<div class=\"card\">\n  <p>Hi</p>\n</div>",
            "import os\nprint(os.getcwd())",
            "if (a == b) {\n    return;\n}",
            "#include <stdio.h>\nint main(void) {\n  printf(\"hi\");\n}",
        ] {
            assert!(looks_like_code(s), "{s:?}");
        }
    }

    #[test]
    fn prose_is_not_code() {
        for s in [
            "",
            "Albuquerque",
            "831 310",
            "Hello (world);",
            "I think a == b is wrong here.",
            "Привет! Как дела? Созвонимся завтра в 10:00 (по Москве).",
            "Buy:\n- milk;\n- bread;\n- eggs;",
            "Dear Anna,\n\nThanks for the notes; I will send the draft tomorrow.\nBest,\nTom",
            "  Roses are red,\n  violets are blue,\n  sugar is sweet,\n  and so are you.",
            "If you want to go, let me know.\nFor now, I'm staying home.",
            "Meeting notes: budget -> approved",
            "1. Open the app\n2. Press ⌘C\n3. Done",
        ] {
            assert!(!looks_like_code(s), "{s:?}");
        }
    }
}
