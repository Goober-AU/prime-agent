//! Port of packages/coding-agent/src/core/tools/ipython-cell-code.ts

use std::sync::OnceLock;

use regex::Regex;

fn bash_cell_magic_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"^(?:[ \t]*\r?\n)*[ \t]*%%bash\b[^\r\n]*(?:\r?\n|$)").expect("valid bash cell pattern")
    })
}

pub struct ParsedIpythonBashCell {
    pub body: String,
}

pub fn parse_ipython_bash_cell(code: &str) -> Option<ParsedIpythonBashCell> {
    let found = bash_cell_magic_pattern().find(code)?;
    Some(ParsedIpythonBashCell {
        body: code[found.end()..].to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_leading_magic_line() {
        let parsed = parse_ipython_bash_cell("%%bash\necho hi\n").expect("cell");
        assert_eq!(parsed.body, "echo hi\n");
    }

    #[test]
    fn parses_blank_lines_before_magic_and_trailing_flags() {
        let parsed = parse_ipython_bash_cell("\n  \n\t%%bash -e\necho hi").expect("cell");
        assert_eq!(parsed.body, "echo hi");
    }

    #[test]
    fn ignores_magic_not_at_the_start() {
        assert!(parse_ipython_bash_cell("print(1)\n%%bash\n").is_none());
        assert!(parse_ipython_bash_cell("%%bashx\n").is_none());
    }

    #[test]
    fn parses_magic_without_trailing_newline() {
        let parsed = parse_ipython_bash_cell("%%bash").expect("cell");
        assert_eq!(parsed.body, "");
    }
}
