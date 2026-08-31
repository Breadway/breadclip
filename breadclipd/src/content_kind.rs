//! Heuristic classification of clipboard text, used to tag the
//! `bread.clip.copied` event's `kind` field (see `bread-shared`'s namespace
//! docs). Deliberately simple pattern matching, not a real parser or ML
//! classifier — good enough to auto-tag "you just copied a stack trace" or
//! "you just copied a URL" for a Lua automation module to react to, not a
//! source of truth anyone should build a security decision on.

/// Checked in this order because a false-not-`error` or false-not-`url` is
/// cheap to live with, while classifying a URL as generic `code` because it
/// contains a `/` would be a worse user-facing miss.
pub fn detect(text: &str) -> &'static str {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return "plain";
    }
    if looks_like_error(trimmed) {
        "error"
    } else if looks_like_url(trimmed) {
        "url"
    } else if looks_like_path(trimmed) {
        "path"
    } else if looks_like_code(trimmed) {
        "code"
    } else {
        "plain"
    }
}

fn looks_like_url(text: &str) -> bool {
    // Single "word" (no internal whitespace) is required — a sentence that
    // merely contains a URL should classify as plain/error/code on its own
    // merits, not get tagged "url" just because a link appears in it.
    if text.split_whitespace().count() != 1 {
        return false;
    }
    let lower = text.to_ascii_lowercase();
    lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("ftp://")
        || lower.starts_with("file://")
        || lower.starts_with("mailto:")
}

fn looks_like_error(text: &str) -> bool {
    const MARKERS: [&str; 8] = [
        "traceback (most recent call last)",
        "panicked at",
        "exception in thread",
        "unhandled exception",
        "stack trace:",
        "at java.",
        "uncaught exception",
        "fatal error:",
    ];
    let lower = text.to_ascii_lowercase();
    if MARKERS.iter().any(|m| lower.contains(m)) {
        return true;
    }
    // A line starting "Error:"/"error:" combined with more than one line is
    // a common shape for compiler/runtime error output; a single bare line
    // like "error: 42" out of context is more likely just plain text.
    let first_line = lower.lines().next().unwrap_or("");
    text.lines().count() > 1
        && (first_line.starts_with("error:") || first_line.starts_with("error["))
}

fn looks_like_path(text: &str) -> bool {
    if text.split_whitespace().count() != 1 {
        return false;
    }
    let has_extension_or_depth = text
        .rsplit('/')
        .next()
        .map(|s| s.contains('.'))
        .unwrap_or(false)
        || text.matches('/').count() >= 2;
    (text.starts_with('/') || text.starts_with("~/") || text.starts_with("./"))
        && has_extension_or_depth
}

fn looks_like_code(text: &str) -> bool {
    const CODE_TOKENS: [&str; 14] = [
        "fn ",
        "function ",
        "def ",
        "class ",
        "const ",
        "let ",
        "import ",
        "#include",
        "public ",
        "private ",
        "=> {",
        "};",
        "</",
        "><",
    ];
    let lines: Vec<&str> = text.lines().collect();
    let token_hits = CODE_TOKENS.iter().filter(|t| text.contains(*t)).count();
    let brace_or_semicolon_lines = lines
        .iter()
        .filter(|l| {
            let t = l.trim_end();
            t.ends_with(';') || t.ends_with('{') || t.ends_with('}')
        })
        .count();

    // Multi-line with a meaningful fraction of "code-shaped" lines, or a
    // couple of code tokens anywhere, counts as code.
    if lines.len() > 1 {
        return token_hits >= 2 || brace_or_semicolon_lines * 2 >= lines.len();
    }

    // A single line only counts as code if it actually *looks* like a code
    // statement — a prose sentence that merely contains a keyword ("let me
    // show you", "function of time", "class is a concept") must stay plain.
    if lines.len() == 1 && token_hits >= 1 && text.len() < 200 {
        let trimmed = text.trim_start();
        let import_like = trimmed.starts_with("import ") || trimmed.starts_with("#include");
        let code_shaped = trimmed.ends_with('{')
            || trimmed.ends_with('}')
            || trimmed.ends_with(';')
            || trimmed.ends_with('(')
            || trimmed.ends_with(')')
            || trimmed.contains("=>")
            || (trimmed.contains('(') && trimmed.contains(')'))
            || trimmed.contains(" = ");
        return import_like || code_shaped;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_url() {
        assert_eq!(detect("https://example.com/path?query=1"), "url");
        assert_eq!(detect("http://localhost:8080"), "url");
        assert_eq!(detect("  https://example.com  "), "url");
    }

    #[test]
    fn does_not_tag_sentence_containing_a_url_as_url() {
        assert_eq!(detect("check out https://example.com later"), "plain");
    }

    #[test]
    fn detects_python_traceback() {
        let text = "Traceback (most recent call last):\n  File \"x.py\", line 1\nValueError: bad";
        assert_eq!(detect(text), "error");
    }

    #[test]
    fn detects_rust_panic() {
        assert_eq!(
            detect("thread 'main' panicked at src/main.rs:10:5:\nindex out of bounds"),
            "error"
        );
    }

    #[test]
    fn detects_multiline_error_prefixed_output() {
        assert_eq!(detect("error: expected `;`\n --> src/main.rs:2:1"), "error");
    }

    #[test]
    fn single_line_error_word_without_context_is_plain() {
        assert_eq!(detect("error: something"), "plain");
    }

    #[test]
    fn detects_absolute_path() {
        assert_eq!(detect("/home/user/.config/bread/init.lua"), "path");
        assert_eq!(detect("~/Projects/bread/README.md"), "path");
    }

    #[test]
    fn detects_multiline_code() {
        let text = "fn main() {\n    let x = 1;\n    println!(\"{}\", x);\n}";
        assert_eq!(detect(text), "code");
    }

    #[test]
    fn detects_single_line_import() {
        assert_eq!(detect("import numpy as np"), "code");
    }

    #[test]
    fn single_line_prose_containing_a_keyword_is_plain() {
        assert_eq!(detect("let me show you something"), "plain");
        assert_eq!(detect("function of time"), "plain");
        assert_eq!(detect("class is a concept"), "plain");
        assert_eq!(detect("const means constant"), "plain");
        assert_eq!(detect("def is short for define"), "plain");
    }

    #[test]
    fn plain_prose_is_plain() {
        assert_eq!(
            detect("just a normal sentence someone copied from an email"),
            "plain"
        );
    }

    #[test]
    fn empty_or_whitespace_is_plain() {
        assert_eq!(detect(""), "plain");
        assert_eq!(detect("   \n  "), "plain");
    }
}
