use codex_protocol::models::FunctionCallOutputContentItem;
use codex_utils_output_truncation::approx_token_count;
use pretty_assertions::assert_eq;

use super::ActionableState;
use super::ActionableStateEntry;
use super::ActionableStateStatus;
use super::RequiredFollowUp;
use super::WriteStdinFollowUpArguments;
use crate::unified_exec::MAX_UNIFIED_EXEC_PROCESSES;

fn entry(process_id: i32, state: ActionableStateStatus) -> ActionableStateEntry {
    ActionableStateEntry {
        session_id: process_id,
        process_id,
        chunk_id: format!("{process_id:06x}"),
        state,
        exit_code: (state == ActionableStateStatus::Completed).then_some(0),
        required_follow_up: (state == ActionableStateStatus::Running).then(|| RequiredFollowUp {
            operation: "write_stdin".to_string(),
            arguments: WriteStdinFollowUpArguments {
                session_id: process_id,
                chars: String::new(),
            },
        }),
    }
}

#[test]
fn actionable_state_is_bounded_without_discarding_a_running_handle() {
    let running_process_id = 10_000;
    let mut entries = (0..MAX_UNIFIED_EXEC_PROCESSES)
        .map(|process_id| entry(process_id as i32, ActionableStateStatus::Completed))
        .collect::<Vec<_>>();
    entries.push(entry(running_process_id, ActionableStateStatus::Running));

    let state = ActionableState::new(entries).expect("bounded actionable state");

    assert_eq!(state.entries.len(), MAX_UNIFIED_EXEC_PROCESSES);
    assert!(
        state
            .entries
            .iter()
            .any(|entry| entry.process_id == running_process_id)
    );
    let Some(output_items) = state.output_items() else {
        panic!("bounded actionable state should render");
    };
    assert_eq!(output_items.len(), MAX_UNIFIED_EXEC_PROCESSES);
    let mut total_tokens = 0;
    for output_item in output_items {
        let FunctionCallOutputContentItem::InputText { text } = output_item else {
            panic!("actionable state should render as text");
        };
        let item_tokens = approx_token_count(&text);
        total_tokens += item_tokens;
        assert!(
            item_tokens <= 1_000,
            "one actionable-state context item crossed the 1K-token review threshold"
        );
    }
    assert!(
        total_tokens <= 10_000,
        "aggregate actionable state is capped"
    );
}
