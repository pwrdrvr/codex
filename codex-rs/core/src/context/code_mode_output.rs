use codex_protocol::models::FunctionCallOutputContentItem;
use serde::Serialize;

use super::ContextualUserFragment;

const ACTIONABLE_STATE_START: &str = "<codex_actionable_state>";
const ACTIONABLE_STATE_END: &str = "</codex_actionable_state>";
pub(crate) const CODE_MODE_OUTPUT_REDUCTION_GUIDANCE: &str = concat!(
    "Codex guidance: output reduction occurs only after the cell completes and does not change ",
    "nested tool results inside JavaScript. Keep broad, independent Code Mode operations batched ",
    "with `Promise.all`, inspect or transform their results in the cell, and emit one compact ",
    "combined result. Use reduced summaries to triage; retrieve only the selected results that ",
    "need deeper inspection, preferably together in a later batch."
);

/// One bounded, Codex-owned continuation handle injected into Code Mode output.
///
/// A fragment contains exactly one handle so the maximum unified-exec process
/// count cannot create a single context item above the 1K-token review limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodeModeActionableStateFragment {
    body: String,
}

#[derive(Serialize)]
struct ActionableStateBody<'a> {
    version: u32,
    entries: [ActionableStateEntry<'a>; 1],
}

#[derive(Serialize)]
struct ActionableStateEntry<'a> {
    session_id: i32,
    process_id: i32,
    chunk_id: &'a str,
    state: &'static str,
    exit_code: Option<i32>,
    required_follow_up: Option<RequiredFollowUp>,
}

#[derive(Serialize)]
struct RequiredFollowUp {
    operation: &'static str,
    arguments: WriteStdinArguments,
}

#[derive(Serialize)]
struct WriteStdinArguments {
    session_id: i32,
    chars: &'static str,
}

impl CodeModeActionableStateFragment {
    pub(crate) fn running(
        version: u32,
        session_id: i32,
        process_id: i32,
        chunk_id: &str,
    ) -> Option<Self> {
        Self::new(ActionableStateBody {
            version,
            entries: [ActionableStateEntry {
                session_id,
                process_id,
                chunk_id,
                state: "running",
                exit_code: None,
                required_follow_up: Some(RequiredFollowUp {
                    operation: "write_stdin",
                    arguments: WriteStdinArguments {
                        session_id,
                        chars: "",
                    },
                }),
            }],
        })
    }

    pub(crate) fn completed(
        version: u32,
        session_id: i32,
        process_id: i32,
        chunk_id: &str,
        exit_code: i32,
    ) -> Option<Self> {
        Self::new(ActionableStateBody {
            version,
            entries: [ActionableStateEntry {
                session_id,
                process_id,
                chunk_id,
                state: "completed",
                exit_code: Some(exit_code),
                required_follow_up: None,
            }],
        })
    }

    fn new(body: ActionableStateBody<'_>) -> Option<Self> {
        let chunk_id = body.entries[0].chunk_id;
        if chunk_id.len() != 6 || !chunk_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        let body = serde_json::to_string(&body).ok()?;
        // All variable-width fields are fixed-width integers or the validated
        // six-byte chunk ID. Keep the assertion explicit at the fragment seam.
        (body.len() <= 512).then_some(Self { body })
    }

    pub(crate) fn into_output_item(self) -> FunctionCallOutputContentItem {
        FunctionCallOutputContentItem::InputText {
            text: self.render(),
        }
    }
}

impl ContextualUserFragment for CodeModeActionableStateFragment {
    fn role(&self) -> &'static str {
        "user"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        (ACTIONABLE_STATE_START, ACTIONABLE_STATE_END)
    }

    fn body(&self) -> String {
        self.body.clone()
    }
}

/// Trusted, constant-size guidance added after a host-selected replacement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CodeModeOutputReductionGuidance;

impl ContextualUserFragment for CodeModeOutputReductionGuidance {
    fn role(&self) -> &'static str {
        "user"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("", "")
    }

    fn body(&self) -> String {
        CODE_MODE_OUTPUT_REDUCTION_GUIDANCE.to_string()
    }
}
