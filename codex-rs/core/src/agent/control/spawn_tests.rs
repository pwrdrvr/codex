use codex_history::RolloutItem;
use codex_history::TokenMiserDecisionRecord;
use codex_history::TokenMiserOutput;
use codex_history::TokenMiserStoredOutcome;
use codex_protocol::ThreadId;
use codex_protocol::models::FunctionCallOutputContentItem;
use std::sync::Arc;

use super::keep_forked_rollout_item;

#[test]
fn fork_filters_token_miser_internal_objects_in_every_history_mode() {
    let thread_id = ThreadId::new();
    let raw = TokenMiserOutput {
        version: 1,
        object_id: "opaque-object".to_string(),
        thread_id,
        turn_id: "turn".to_string(),
        call_id: "call".to_string(),
        cell_id: "cell".to_string(),
        script_status: "Script completed".to_string(),
        success: Some(true),
        content_items: vec![FunctionCallOutputContentItem::InputText {
            text: "raw secret".to_string(),
        }],
    };
    let decision = TokenMiserDecisionRecord {
        version: 1,
        object_id: raw.object_id.clone(),
        thread_id,
        turn_id: raw.turn_id.clone(),
        call_id: raw.call_id.clone(),
        cell_id: raw.cell_id.clone(),
        outcome: TokenMiserStoredOutcome::Hide {
            reason: "hidden".to_string(),
        },
        usage: None,
    };

    for preserve_reference_context_item in [false, true] {
        assert!(!keep_forked_rollout_item(
            &RolloutItem::TokenMiserOutput(Arc::new(raw.clone())),
            preserve_reference_context_item,
        ));
        assert!(!keep_forked_rollout_item(
            &RolloutItem::TokenMiserDecision(decision.clone()),
            preserve_reference_context_item,
        ));
    }
}
