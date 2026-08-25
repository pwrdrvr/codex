use codex_protocol::ResponseItemId;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ReasoningItemContent;
use codex_protocol::models::ReasoningItemReasoningSummary;
use codex_protocol::models::ResponseItem;
use pretty_assertions::assert_eq;

use super::MAX_PARENT_INTENT_CHARS;
use super::from_response_item;

fn assistant_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: Some(ResponseItemId::with_suffix("msg", "intent")),
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

#[test]
fn returns_visible_assistant_narration() {
    let intent = from_response_item(
        &assistant_message("I’ll inspect the failing checks now."),
        /*plan_mode*/ false,
    );

    assert_eq!(
        intent.as_deref(),
        Some("I’ll inspect the failing checks now.")
    );
}

#[test]
fn retains_the_most_recent_unicode_scalar_characters() {
    let text = format!("discard-me{}", "🦀".repeat(MAX_PARENT_INTENT_CHARS));
    let intent = from_response_item(&assistant_message(&text), /*plan_mode*/ false)
        .expect("assistant narration should be captured");

    assert_eq!(intent.chars().count(), MAX_PARENT_INTENT_CHARS);
    assert_eq!(intent.as_ref(), "🦀".repeat(MAX_PARENT_INTENT_CHARS));
}

#[test]
fn ignores_reasoning_summary_and_raw_chain_of_thought() {
    let reasoning = ResponseItem::Reasoning {
        id: Some(ResponseItemId::with_suffix("rs", "secret")),
        summary: vec![ReasoningItemReasoningSummary::SummaryText {
            text: "hidden summary".to_string(),
        }],
        content: Some(vec![ReasoningItemContent::ReasoningText {
            text: "private chain of thought".to_string(),
        }]),
        encrypted_content: None,
        internal_chat_message_metadata_passthrough: None,
    };

    assert_eq!(from_response_item(&reasoning, /*plan_mode*/ false), None);
}

#[test]
fn ignores_messages_without_visible_assistant_narration() {
    let user_message = ResponseItem::Message {
        id: Some(ResponseItemId::with_suffix("msg", "user")),
        role: "user".to_string(),
        content: vec![ContentItem::OutputText {
            text: "not assistant narration".to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    };

    assert_eq!(from_response_item(&user_message, /*plan_mode*/ false), None);
}
