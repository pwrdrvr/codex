use std::sync::Arc;

use codex_protocol::models::ResponseItem;

use crate::stream_events_utils::last_assistant_message_from_item;

pub(crate) const MAX_PARENT_INTENT_CHARS: usize = 4_000;

/// Returns the bounded, model-visible assistant narration carried by `item`.
///
/// `last_assistant_message_from_item` accepts only assistant message output text
/// and strips hidden assistant markup. In particular, reasoning items and their
/// summaries are never considered here.
pub(crate) fn from_response_item(item: &ResponseItem, plan_mode: bool) -> Option<Arc<str>> {
    let text = last_assistant_message_from_item(item, plan_mode)?;
    let start = text
        .char_indices()
        .rev()
        .nth(MAX_PARENT_INTENT_CHARS.saturating_sub(1))
        .map_or(0, |(index, _)| index);
    Some(Arc::from(&text[start..]))
}

#[cfg(test)]
#[path = "parent_intent_tests.rs"]
mod tests;
