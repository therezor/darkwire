//! Message constructors and accessors.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use darkwire_core::messages::{
    AssistantOptions, FileDetails, ImageSource, ToolOptions, assistant_message, file_part,
    has_images, image_part, system_message, text_of, text_part, tool_message, user_message,
    without_images,
};
use darkwire_protocol::{ChatMessage, ContentPart, ToolCall};
use serde_json::{Value, json};

fn png() -> ContentPart {
    image_part("image/png", ImageSource::Data("aGVsbG8=".to_owned()))
}

fn attached() -> ContentPart {
    file_part(
        "uploads/ab12cd34-scan.pdf",
        "application/pdf",
        FileDetails {
            name: Some("scan.pdf".to_owned()),
            size_bytes: Some(2048),
        },
    )
}

fn shape(value: &impl serde::Serialize) -> Value {
    serde_json::to_value(value).unwrap()
}

fn call() -> ToolCall {
    ToolCall {
        id: "a".to_owned(),
        name: "read_file".to_owned(),
        arguments_json: "{}".to_owned(),
    }
}

mod content_parts {
    use super::*;

    #[test]
    fn builds_a_text_part() {
        assert_eq!(
            shape(&text_part("hi")),
            json!({"type": "text", "text": "hi"})
        );
    }

    #[test]
    fn builds_an_inline_image_part() {
        assert_eq!(
            shape(&png()),
            json!({"type": "image", "mimeType": "image/png", "data": "aGVsbG8="})
        );
    }

    #[test]
    fn builds_a_referenced_image_part() {
        assert_eq!(
            shape(&image_part(
                "image/jpeg",
                ImageSource::Url("https://host/signed".to_owned())
            )),
            json!({"type": "image", "mimeType": "image/jpeg", "url": "https://host/signed"})
        );
    }

    #[test]
    fn builds_a_file_part() {
        assert_eq!(
            shape(&attached()),
            json!({
                "type": "file",
                "mimeType": "application/pdf",
                "path": "uploads/ab12cd34-scan.pdf",
                "name": "scan.pdf",
                "sizeBytes": 2048
            })
        );
    }

    #[test]
    fn omits_absent_file_details_rather_than_writing_null() {
        // This part is persisted verbatim, so an absent detail must be absent.
        assert_eq!(
            shape(&file_part(
                "uploads/x.bin",
                "application/octet-stream",
                FileDetails::default()
            )),
            json!({"type": "file", "mimeType": "application/octet-stream", "path": "uploads/x.bin"})
        );
    }
}

mod constructors {
    use super::*;

    #[test]
    fn builds_a_system_message() {
        assert_eq!(
            shape(&system_message("you are a ghost")),
            json!({"role": "system", "content": "you are a ghost"})
        );
    }

    #[test]
    fn wraps_a_plain_string_as_a_user_message() {
        assert_eq!(
            shape(&user_message("hello")),
            json!({"role": "user", "content": [{"type": "text", "text": "hello"}]})
        );
    }

    #[test]
    fn accepts_explicit_parts_for_multimodal_input() {
        let message = user_message(vec![text_part("look"), png()]);
        assert_eq!(message.content, vec![text_part("look"), png()]);
    }

    #[test]
    fn owns_its_parts() {
        let mut parts = vec![text_part("a")];
        let message = user_message(parts.clone());
        parts.push(text_part("b"));
        assert_eq!(message.content.len(), 1);
    }

    #[test]
    fn defaults_an_assistant_message_to_no_tool_calls() {
        assert_eq!(
            shape(&assistant_message("done", AssistantOptions::default())),
            json!({
                "role": "assistant",
                "content": [{"type": "text", "text": "done"}],
                "toolCalls": []
            })
        );
    }

    #[test]
    fn carries_tool_calls_and_reasoning() {
        let message = assistant_message(
            "",
            AssistantOptions {
                tool_calls: vec![call()],
                reasoning: Some("thinking".to_owned()),
            },
        );
        assert_eq!(message.tool_calls, vec![call()]);
        assert_eq!(message.reasoning.as_deref(), Some("thinking"));
    }

    #[test]
    fn omits_reasoning_entirely_when_there_is_none() {
        let value = shape(&assistant_message("done", AssistantOptions::default()));
        assert!(value.get("reasoning").is_none());
    }

    #[test]
    fn defaults_a_tool_message_to_a_successful_untruncated_result() {
        assert_eq!(
            shape(&tool_message(
                "a",
                "read_file",
                "contents",
                ToolOptions::default()
            )),
            json!({
                "role": "tool",
                "toolCallId": "a",
                "name": "read_file",
                "content": "contents",
                "isError": false,
                "truncated": false
            })
        );
    }

    #[test]
    fn flags_a_failed_tool_result_explicitly() {
        let message = tool_message(
            "a",
            "exec",
            "Error: nope",
            ToolOptions {
                is_error: true,
                truncated: false,
            },
        );
        assert!(message.is_error);
    }
}

mod text_of_tests {
    use super::*;

    #[test]
    fn reads_a_system_message() {
        assert_eq!(text_of(&system_message("prompt").into()), "prompt");
    }

    #[test]
    fn reads_a_tool_message() {
        assert_eq!(
            text_of(&tool_message("a", "t", "output", ToolOptions::default()).into()),
            "output"
        );
    }

    #[test]
    fn reads_a_single_part_message() {
        assert_eq!(text_of(&user_message("hello").into()), "hello");
    }

    #[test]
    fn joins_several_parts_with_a_newline_rather_than_gluing_them() {
        let message = assistant_message(
            vec![text_part("first"), text_part("second")],
            AssistantOptions::default(),
        );
        assert_eq!(text_of(&message.into()), "first\nsecond");
    }

    #[test]
    fn drops_image_parts() {
        let message = user_message(vec![text_part("look at this"), png()]);
        assert_eq!(text_of(&message.into()), "look at this");
    }

    #[test]
    fn is_empty_for_an_image_only_message() {
        assert_eq!(text_of(&user_message(vec![png()]).into()), "");
    }

    #[test]
    fn is_empty_for_an_empty_message() {
        assert_eq!(text_of(&user_message(Vec::new()).into()), "");
    }

    #[test]
    fn ignores_file_parts() {
        // Deliberate: this feeds the session title, and a session named after a
        // mangled upload path is worse than one left untitled.
        assert_eq!(
            text_of(&user_message(vec![text_part("summarise"), attached()]).into()),
            "summarise"
        );
        assert_eq!(text_of(&user_message(vec![attached()]).into()), "");
    }
}

mod has_images_tests {
    use super::*;

    #[test]
    fn detects_an_image_part() {
        assert!(has_images(
            &user_message(vec![text_part("x"), png()]).into()
        ));
    }

    #[test]
    fn is_false_for_text_only_and_for_roles_that_cannot_carry_parts() {
        assert!(!has_images(&user_message("x").into()));
        assert!(!has_images(&system_message("x").into()));
        assert!(!has_images(
            &tool_message("a", "t", "x", ToolOptions::default()).into()
        ));
    }

    #[test]
    fn is_false_for_an_unmaterialised_file_part() {
        // If this were true, the strip-images degradation could fire on a
        // request whose attachments have not been read yet and delete them.
        let image = file_part(
            "uploads/ab12cd34-shot.png",
            "image/png",
            FileDetails::default(),
        );
        assert!(!has_images(
            &user_message(vec![text_part("x"), image]).into()
        ));
    }
}

mod without_images_tests {
    use super::*;

    fn parts_of(message: &ChatMessage) -> &[ContentPart] {
        match message {
            ChatMessage::User(user) => &user.content,
            ChatMessage::Assistant(assistant) => &assistant.content,
            other => panic!("expected a message with parts, got {other:?}"),
        }
    }

    #[test]
    fn strips_images_and_keeps_the_text() {
        let stripped = without_images(user_message(vec![text_part("look"), png()]).into());
        assert_eq!(parts_of(&stripped), &[text_part("look")]);
    }

    #[test]
    fn returns_the_message_unchanged_when_there_is_nothing_to_strip() {
        let message: ChatMessage = user_message("plain").into();
        assert_eq!(without_images(message.clone()), message);
    }

    #[test]
    fn leaves_roles_that_cannot_carry_images_alone() {
        let system: ChatMessage = system_message("x").into();
        let tool: ChatMessage = tool_message("a", "t", "x", ToolOptions::default()).into();
        assert_eq!(without_images(system.clone()), system);
        assert_eq!(without_images(tool.clone()), tool);
    }

    #[test]
    fn keeps_file_parts_which_are_not_images() {
        // The degradation removes images because a model rejected one. An
        // attachment reference is not what it rejected.
        let stripped =
            without_images(user_message(vec![text_part("look"), png(), attached()]).into());
        assert_eq!(parts_of(&stripped), &[text_part("look"), attached()]);
    }

    #[test]
    fn preserves_tool_calls_while_stripping_images() {
        let stripped = without_images(
            assistant_message(
                vec![text_part("x"), png()],
                AssistantOptions {
                    tool_calls: vec![call()],
                    reasoning: None,
                },
            )
            .into(),
        );
        match stripped {
            ChatMessage::Assistant(assistant) => {
                assert_eq!(assistant.tool_calls, vec![call()]);
                assert_eq!(assistant.content, vec![text_part("x")]);
            }
            other => panic!("expected an assistant message, got {other:?}"),
        }
    }
}
