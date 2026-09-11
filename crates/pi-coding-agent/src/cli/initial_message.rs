//! Port of packages/coding-agent/src/cli/initial-message.ts

use pi_ai::types::ImageContent;

use super::args::Args;

pub struct InitialMessageInput<'a> {
    pub parsed: &'a mut Args,
    pub file_text: Option<String>,
    pub file_images: Option<Vec<ImageContent>>,
    pub stdin_content: Option<String>,
}

pub struct InitialMessageResult {
    pub initial_message: Option<String>,
    pub initial_images: Option<Vec<ImageContent>>,
}

/// Combine stdin content, @file text, and the first CLI message into a single
/// initial prompt for non-interactive mode.
pub fn build_initial_message(input: InitialMessageInput<'_>) -> InitialMessageResult {
    let InitialMessageInput { parsed, file_text, file_images, stdin_content } = input;
    let mut parts: Vec<String> = Vec::new();
    if let Some(stdin_content) = stdin_content {
        parts.push(stdin_content);
    }
    if let Some(file_text) = file_text {
        if !file_text.is_empty() {
            parts.push(file_text);
        }
    }

    if !parsed.messages.is_empty() {
        parts.push(parsed.messages[0].clone());
        parsed.messages.remove(0);
    }

    let initial_images = match file_images {
        Some(images) if !images.is_empty() => Some(images),
        _ => None,
    };

    InitialMessageResult {
        initial_message: if parts.is_empty() { None } else { Some(parts.join("\n\n")) },
        initial_images,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args_with_messages(messages: &[&str]) -> Args {
        let mut args = Args::default();
        args.messages = messages.iter().map(|message| message.to_string()).collect();
        args
    }

    #[test]
    fn joins_stdin_file_text_and_the_first_message() {
        let mut args = args_with_messages(&["first", "second"]);
        let result = build_initial_message(InitialMessageInput {
            parsed: &mut args,
            file_text: Some("file body".to_string()),
            file_images: None,
            stdin_content: Some("stdin body".to_string()),
        });
        assert_eq!(result.initial_message.as_deref(), Some("stdin body\n\nfile body\n\nfirst"));
        assert_eq!(args.messages, vec!["second".to_string()]);
        assert_eq!(result.initial_images, None);
    }

    #[test]
    fn returns_none_when_every_part_is_missing() {
        let mut args = args_with_messages(&[]);
        let result = build_initial_message(InitialMessageInput {
            parsed: &mut args,
            file_text: None,
            file_images: None,
            stdin_content: None,
        });
        assert_eq!(result.initial_message, None);
    }

    #[test]
    fn empty_file_text_is_skipped_like_a_falsy_string() {
        let mut args = args_with_messages(&[]);
        let result = build_initial_message(InitialMessageInput {
            parsed: &mut args,
            file_text: Some(String::new()),
            file_images: None,
            stdin_content: Some("stdin".to_string()),
        });
        assert_eq!(result.initial_message.as_deref(), Some("stdin"));
    }

    #[test]
    fn empty_stdin_content_is_still_a_part() {
        let mut args = args_with_messages(&[]);
        let result = build_initial_message(InitialMessageInput {
            parsed: &mut args,
            file_text: None,
            file_images: None,
            stdin_content: Some(String::new()),
        });
        assert_eq!(result.initial_message.as_deref(), Some(""));
    }

    #[test]
    fn images_are_returned_only_when_present() {
        let mut args = args_with_messages(&[]);
        let images = vec![ImageContent::new("data", "image/png")];
        let result = build_initial_message(InitialMessageInput {
            parsed: &mut args,
            file_text: None,
            file_images: Some(images),
            stdin_content: None,
        });
        assert_eq!(result.initial_images.as_ref().map(Vec::len), Some(1));

        let mut args = args_with_messages(&[]);
        let result = build_initial_message(InitialMessageInput {
            parsed: &mut args,
            file_text: None,
            file_images: Some(Vec::new()),
            stdin_content: None,
        });
        assert_eq!(result.initial_images, None);
    }
}
