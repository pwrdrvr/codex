use codex_history::RolloutItem;
use codex_history::TokenMiserDecisionRecord;
use codex_history::TokenMiserOutput;
use codex_history::TokenMiserStoredOutcome;
use codex_protocol::ThreadId;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_utils_output_truncation::approx_token_count;
use pretty_assertions::assert_eq;

use super::reducer::MAX_REDUCER_INPUT_BYTES;
use super::reducer::reducer_input;
use super::*;

const OBJECT_ID: &str = "00000000-0000-4000-8000-000000000001";

fn thread_id(value: u128) -> ThreadId {
    ThreadId::from_string(&Uuid::from_u128(value).to_string()).expect("thread id")
}

fn raw_output(thread_id: ThreadId, text: String) -> TokenMiserOutput {
    TokenMiserOutput {
        version: TOKEN_MISER_VERSION,
        object_id: OBJECT_ID.to_string(),
        thread_id,
        turn_id: "turn-id".to_string(),
        call_id: "call-id".to_string(),
        cell_id: "cell-id".to_string(),
        script_status: "Script completed".to_string(),
        success: Some(true),
        content_items: vec![FunctionCallOutputContentItem::InputText { text }],
    }
}

fn reduction_context() -> ReductionContext {
    ReductionContext {
        thread_id: "thread-id".to_string(),
        turn_id: "turn-id".to_string(),
        call_id: "call-id".to_string(),
        cell_id: "cell-id".to_string(),
        script: Some("text(result)".to_string()),
        parent_intent: None,
        actionable_state: None,
        script_status: "Script completed".to_string(),
    }
}

#[test]
fn reducer_input_is_hard_bounded_and_omits_the_middle_of_large_text() {
    let dense = "\u{10ffff}\0".repeat(40_000);
    let text = format!("HEAD{dense}MIDDLE-SECRET{dense}TAIL");
    let raw = raw_output(thread_id(1), text);

    let rendered = reducer_input(&reduction_context(), &raw, 16 * 1024)
        .expect("bounded reducer input should render");

    assert!(rendered.len() <= MAX_REDUCER_INPUT_BYTES);
    assert!(approx_token_count(&rendered) < 1_000);
    assert!(rendered.contains("HEAD"));
    assert!(rendered.contains("TAIL"));
    assert!(!rendered.contains("MIDDLE-SECRET"));
    assert!(!rendered.contains("parent_intent"));
}

#[test]
fn retrieval_is_exact_bounded_and_thread_scoped() {
    let owner = thread_id(2);
    let text = format!("first needle\n{}\nlast needle", "界".repeat(20_000));
    let raw = raw_output(owner, text.clone());
    let service =
        TokenMiserService::new([RolloutItem::TokenMiserOutput(Arc::new(raw))], Some(owner));

    let first = service
        .read(owner, OBJECT_ID, 0, 0, usize::MAX)
        .expect("bounded read");
    assert!(first["content"].as_str().expect("text").len() <= MAX_READ_BYTES);
    assert!(first["next_offset"].as_u64().is_some());
    let search = service
        .search(owner, OBJECT_ID, "needle", usize::MAX)
        .expect("bounded search");
    assert_eq!(search["matches"].as_array().expect("matches").len(), 2);
    assert!(search.to_string().len() <= MAX_READ_BYTES);
    assert_eq!(
        service.read(thread_id(3), OBJECT_ID, 0, 0, 16),
        Err("Token Miser output was not found in this session".to_string())
    );

    let whole = service
        .read(owner, OBJECT_ID, 0, 0, text.len())
        .expect("read remains capped");
    assert_ne!(whole["content"].as_str(), Some(text.as_str()));

    let adversarial_text = (0..MAX_SEARCH_RESULTS)
        .map(|_| format!("needle{}\n", "\0".repeat(MAX_SEARCH_SNIPPET_BYTES)))
        .collect::<String>();
    let adversarial = TokenMiserService::new(
        [RolloutItem::TokenMiserOutput(Arc::new(raw_output(
            owner,
            adversarial_text,
        )))],
        Some(owner),
    )
    .search(owner, OBJECT_ID, "needle", usize::MAX)
    .expect("escape-dense search remains bounded");
    assert!(adversarial.to_string().len() <= MAX_READ_BYTES);
}

#[test]
fn restored_decision_is_reused_without_an_uncommitted_cell() {
    let owner = thread_id(4);
    let raw = raw_output(owner, "exact".to_string());
    let decision = TokenMiserDecisionRecord {
        version: TOKEN_MISER_VERSION,
        object_id: raw.object_id.clone(),
        thread_id: owner,
        turn_id: raw.turn_id.clone(),
        call_id: raw.call_id.clone(),
        cell_id: raw.cell_id.clone(),
        outcome: TokenMiserStoredOutcome::Replace {
            replacement: "restored replacement".to_string(),
        },
        usage: None,
    };
    let key = terminal_key(&raw);
    let service = TokenMiserService::new(
        [
            RolloutItem::TokenMiserOutput(Arc::new(raw)),
            RolloutItem::TokenMiserDecision(decision),
        ],
        Some(owner),
    );

    let cells = service.terminal_cells.lock().expect("terminal cells");
    let state = cells.get(&key).expect("restored terminal state");
    assert_eq!(state.persistence.get(), Some(&true));
    assert!(matches!(
        state.decision.get(),
        Some(TokenMiserDecision::Replace(replacement)) if replacement == "restored replacement"
    ));
}

#[test]
fn restored_raw_output_without_a_decision_is_not_reduced_again() {
    let owner = thread_id(5);
    let raw = raw_output(owner, "exact".to_string());
    let key = terminal_key(&raw);
    let object_id = raw.object_id.clone();
    let service =
        TokenMiserService::new([RolloutItem::TokenMiserOutput(Arc::new(raw))], Some(owner));

    let cells = service.terminal_cells.lock().expect("terminal cells");
    let state = cells.get(&key).expect("restored terminal state");
    assert_eq!(state.raw.object_id, object_id);
    assert_eq!(state.persistence.get(), Some(&true));
    assert!(matches!(
        state.decision.get(),
        Some(TokenMiserDecision::Hide(reason))
            if reason == "retained; prior reducer decision was unavailable"
    ));
}

#[test]
fn structured_output_storage_bound_rejects_adversarial_oversize_without_serializing() {
    let within = vec![
        FunctionCallOutputContentItem::InputText {
            text: "a\0b".to_string(),
        },
        FunctionCallOutputContentItem::InputImage {
            image_url: "data:image/png;base64,AAEC".to_string(),
            detail: None,
        },
        FunctionCallOutputContentItem::EncryptedContent {
            encrypted_content: "ciphertext".to_string(),
        },
    ];
    let exact_bytes = within
        .iter()
        .map(|item| item_source(item).1.len())
        .sum::<usize>();

    let metadata = ["turn", "call", "cell", "status"];
    let metadata_bytes = metadata.iter().map(|value| value.len()).sum::<usize>();

    assert!(output_fits_storage_bound(
        metadata,
        &within,
        3,
        exact_bytes + metadata_bytes,
    ));
    assert!(!output_fits_storage_bound(
        metadata,
        &within,
        2,
        exact_bytes + metadata_bytes,
    ));
    assert!(!output_fits_storage_bound(
        metadata,
        &within,
        3,
        (exact_bytes + metadata_bytes).saturating_sub(1),
    ));

    assert_eq!(
        MAX_PERSISTED_OUTPUT_SOURCE_BYTES * 8,
        codex_code_mode::host::MAX_FRAME_BYTES
    );
    let adversarial = vec![FunctionCallOutputContentItem::InputText {
        text: "\0".repeat(MAX_PERSISTED_OUTPUT_SOURCE_BYTES),
    }];
    assert!(output_fits_storage_bound(
        std::iter::empty(),
        &adversarial,
        MAX_PERSISTED_OUTPUT_ITEMS,
        MAX_PERSISTED_OUTPUT_SOURCE_BYTES,
    ));
    let oversized = vec![FunctionCallOutputContentItem::InputText {
        text: "\0".repeat(MAX_PERSISTED_OUTPUT_SOURCE_BYTES + 1),
    }];
    assert!(!output_fits_storage_bound(
        std::iter::empty(),
        &oversized,
        MAX_PERSISTED_OUTPUT_ITEMS,
        MAX_PERSISTED_OUTPUT_SOURCE_BYTES,
    ));
    assert!(!output_fits_storage_bound(
        [item_source(&oversized[0]).1],
        &[],
        MAX_PERSISTED_OUTPUT_ITEMS,
        MAX_PERSISTED_OUTPUT_SOURCE_BYTES,
    ));
}
