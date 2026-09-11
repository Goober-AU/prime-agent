//! Port of packages/coding-agent/src/core/skill-blocks.ts
use serde::{Deserialize, Serialize};

/// Parsed skill block from a user message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParsedSkillBlock {
    pub name: String,
    pub location: String,
    pub content: String,
    /// `undefined` when the trailing user message is absent.
    pub user_message: Option<String>,
}

/// Parse a skill block from message text.
/// Returns `None` if the text doesn't contain a skill block.
pub fn parse_skill_block(text: &str) -> Option<ParsedSkillBlock> {
    let pattern = regex::Regex::new(
        r#"(?s)^<skill name="([^"]+)" location="([^"]+)">\n(.*?)\n</skill>(?:\n\n(.+))?$"#,
    )
    .ok()?;
    let captures = pattern.captures(text)?;
    let user_message = captures
        .get(4)
        .map(|value| value.as_str().trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    Some(ParsedSkillBlock {
        name: captures
            .get(1)
            .map(|value| value.as_str())
            .unwrap_or("")
            .to_string(),
        location: captures
            .get(2)
            .map(|value| value.as_str())
            .unwrap_or("")
            .to_string(),
        content: captures
            .get(3)
            .map(|value| value.as_str())
            .unwrap_or("")
            .to_string(),
        user_message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_skill_block_with_a_trailing_user_message() {
        let parsed = parse_skill_block("<skill name=\"goal\" location=\"/s/goal/SKILL.md\">\nBody line\n</skill>\n\nDo the thing\n")
            .expect("parsed");
        assert_eq!(parsed.name, "goal");
        assert_eq!(parsed.location, "/s/goal/SKILL.md");
        assert_eq!(parsed.content, "Body line");
        assert_eq!(parsed.user_message.as_deref(), Some("Do the thing"));
    }

    #[test]
    fn parses_a_skill_block_without_a_user_message() {
        let parsed =
            parse_skill_block("<skill name=\"a\" location=\"b\">\nBody\n</skill>").expect("parsed");
        assert_eq!(parsed.user_message, None);
        let blank = parse_skill_block("<skill name=\"a\" location=\"b\">\nBody\n</skill>\n\n   ")
            .expect("parsed");
        assert_eq!(blank.user_message, None);
    }

    #[test]
    fn returns_none_for_plain_text() {
        assert!(parse_skill_block("just a message").is_none());
        assert!(parse_skill_block("<skill name=\"a\">\nBody\n</skill>").is_none());
    }
}
