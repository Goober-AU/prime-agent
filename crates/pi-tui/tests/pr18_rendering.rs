use std::rc::Rc;

use pi_tui::components::markdown::{Markdown, MarkdownOptions, MarkdownTheme};
use pi_tui::latex::latex_to_unicode;
use pi_tui::tui::Component;
use pi_tui::utils::{strip_ansi, truncate_to_width, visible_width, wrap_text_with_ansi};
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixtures {
    widths: Vec<(String, usize)>,
    latex: Vec<(String, String)>,
    markdown: Vec<(String, f64, Vec<String>)>,
    truncate: Vec<(String, f64, String, bool, String)>,
    wrap: Vec<(String, usize, Vec<String>)>,
    strip: Vec<(String, String)>,
}

fn fixtures() -> Fixtures {
    serde_json::from_str(include_str!("fixtures/pr18_rendering.json")).unwrap()
}

fn markdown(text: &str) -> Markdown {
    fn plain(s: &str) -> String {
        s.to_string()
    }
    let theme = MarkdownTheme {
        heading: Rc::new(plain),
        link: Rc::new(plain),
        link_url: Rc::new(plain),
        code: Rc::new(plain),
        code_block: Rc::new(plain),
        code_block_border: Rc::new(plain),
        quote: Rc::new(plain),
        quote_border: Rc::new(plain),
        hr: Rc::new(plain),
        list_bullet: Rc::new(plain),
        bold: Rc::new(plain),
        italic: Rc::new(plain),
        strikethrough: Rc::new(plain),
        underline: Rc::new(plain),
        highlight_code: None,
        code_block_indent: None,
        math: None,
        math_block: None,
    };
    Markdown::new(
        text.to_string(),
        0,
        0,
        theme,
        None,
        MarkdownOptions::default(),
    )
}

#[test]
fn emoji_flags_modifiers_and_combining_marks_match_typescript() {
    let failures: Vec<_> = fixtures()
        .widths
        .into_iter()
        .filter_map(|(text, expected)| {
            let actual = visible_width(&text);
            (actual != expected).then_some((text, expected, actual))
        })
        .collect();
    assert!(
        failures.is_empty(),
        "{} width mismatches: {:?}",
        failures.len(),
        &failures[..failures.len().min(20)]
    );
}

#[test]
fn latex_symbols_scripts_accents_and_whitespace_match_typescript() {
    for (text, expected) in fixtures().latex {
        assert_eq!(latex_to_unicode(&text), expected, "{text:?}");
    }
}

#[test]
fn inline_display_and_streamed_math_match_typescript() {
    let mut failures = Vec::new();
    for (text, width, expected) in fixtures().markdown {
        let actual: Vec<_> = markdown(&text)
            .render(width)
            .iter()
            .map(|s| strip_ansi(s))
            .collect();
        if actual != expected {
            failures.push((text, width, expected, actual));
        }
    }
    assert!(
        failures.is_empty(),
        "{} markdown mismatches: {:?}",
        failures.len(),
        &failures[..failures.len().min(10)]
    );
}

#[test]
fn truncation_preserves_graphemes_contiguous_prefixes_and_style_boundaries() {
    for (text, width, ellipsis, pad, expected) in fixtures().truncate {
        assert_eq!(
            truncate_to_width(&text, width, &ellipsis, pad),
            expected,
            "{text:?}, width={width}, ellipsis={ellipsis:?}, pad={pad}"
        );
    }
}

#[test]
fn wrapping_preserves_emoji_and_reopens_styles_and_hyperlinks() {
    for (text, width, expected) in fixtures().wrap {
        assert_eq!(
            wrap_text_with_ansi(&text, width),
            expected,
            "{text:?}, width={width}"
        );
    }
}

#[test]
fn malformed_escapes_are_utf8_safe() {
    for (text, expected) in fixtures().strip {
        assert_eq!(strip_ansi(&text), expected, "{text:?}");
    }
}

#[test]
fn huge_unicode_and_malformed_control_strings_have_bounded_truncated_output() {
    for text in [
        "あ".repeat(2_000_000),
        "👩‍💻".repeat(500_000),
        "\x1b]unterminated".repeat(100_000),
    ] {
        let truncated = truncate_to_width(&text, 20.0, "...", true);
        assert_eq!(visible_width(&truncated), 20);
        assert!(truncated.len() < 200);
    }
}

#[test]
fn changing_a_cached_block_preserves_its_new_structure() {
    for (before, after) in [
        ("- before\n\ntail", "- after\n\ntail"),
        ("# same\n\ntail", "### same\n\ntail"),
        (
            "[same](https://a.test)\n\ntail",
            "[same](https://b.test)\n\ntail",
        ),
        ("$$ $$", "$$ $$"),
    ] {
        let mut cached = markdown(before);
        cached.render(40.0);
        cached.set_text(after.to_string());
        assert_eq!(
            cached.render(40.0),
            markdown(after).render(40.0),
            "{before:?} -> {after:?}"
        );
    }
}
