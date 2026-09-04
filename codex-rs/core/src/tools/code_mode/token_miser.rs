use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;

use codex_history::RolloutItem;
use codex_history::TokenMiserDecisionRecord;
use codex_history::TokenMiserOutput;
use codex_history::TokenMiserStoredOutcome;
use codex_protocol::models::FunctionCallOutputContentItem;
use serde_json::Value;
use serde_json::json;
use tokio::sync::OnceCell;
use uuid::Uuid;

use crate::config::InProcessTokenMiserConfig;
use crate::context::CodeModeOutputReplacementFence;
use crate::context::ContextualUserFragment;
use crate::context::TokenMiserReceipt;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;

use super::ReductionContext;

mod reducer;

const TOKEN_MISER_VERSION: u32 = 1;
const MAX_READ_BYTES: usize = 8 * 1024;
const MAX_SEARCH_QUERY_BYTES: usize = 256;
const MAX_SEARCH_RESULTS: usize = 20;
const MAX_SEARCH_SNIPPET_BYTES: usize = 512;
/// The default host also enforces a 64 MiB serialized IPC frame. This smaller decoded-source cap
/// bounds rollout JSON escaping and applies equally to injected or future session providers.
const MAX_PERSISTED_OUTPUT_SOURCE_BYTES: usize = codex_code_mode::host::MAX_FRAME_BYTES / 8;
const MAX_PERSISTED_OUTPUT_ITEMS: usize = 1_024;

type TerminalKey = (String, String, String);

struct OutputState {
    raw: Arc<TokenMiserOutput>,
    persistence: OnceCell<bool>,
    decision: OnceCell<TokenMiserDecision>,
}

pub(super) struct TerminalOutput<'a> {
    pub(super) script_status: &'a str,
    pub(super) success: Option<bool>,
    pub(super) content_items: Vec<FunctionCallOutputContentItem>,
    pub(super) max_output_tokens: Option<usize>,
}

#[derive(Clone)]
enum TokenMiserDecision {
    Passthrough,
    Replace(String),
    Hide(String),
}

#[derive(Clone, Default)]
pub(super) struct TokenMiserService {
    outputs: Arc<RwLock<HashMap<String, Arc<TokenMiserOutput>>>>,
    terminal_cells: Arc<Mutex<HashMap<TerminalKey, Arc<OutputState>>>>,
    explicit_retrieval_cells: Arc<Mutex<HashSet<String>>>,
}

impl TokenMiserService {
    pub(super) fn new(
        restored: impl IntoIterator<Item = RolloutItem>,
        expected_thread_id: Option<codex_protocol::ThreadId>,
    ) -> Self {
        let mut raw_outputs = Vec::new();
        let mut decisions = HashMap::new();
        for item in restored {
            match item {
                RolloutItem::TokenMiserOutput(output) => {
                    raw_outputs.push(output);
                }
                RolloutItem::TokenMiserDecision(decision) => {
                    decisions.insert(decision.object_id.clone(), decision);
                }
                _ => {}
            }
        }
        let mut outputs = HashMap::new();
        let mut terminal_cells = HashMap::new();
        for raw in raw_outputs {
            if expected_thread_id.is_some_and(|thread_id| raw.thread_id != thread_id)
                || raw.version != TOKEN_MISER_VERSION
                || !Uuid::parse_str(&raw.object_id)
                    .is_ok_and(|object_id| object_id.get_version_num() == 4)
                || !output_fits_storage_bound(
                    [
                        raw.turn_id.as_str(),
                        raw.call_id.as_str(),
                        raw.cell_id.as_str(),
                        raw.script_status.as_str(),
                    ],
                    &raw.content_items,
                    MAX_PERSISTED_OUTPUT_ITEMS,
                    MAX_PERSISTED_OUTPUT_SOURCE_BYTES,
                )
            {
                continue;
            }
            outputs.insert(raw.object_id.clone(), Arc::clone(&raw));
            terminal_cells.entry(terminal_key(&raw)).or_insert_with(|| {
                let stored_decision = decisions
                    .get(&raw.object_id)
                    .and_then(|stored| {
                        (stored.thread_id == raw.thread_id
                            && stored.turn_id == raw.turn_id
                            && stored.call_id == raw.call_id
                            && stored.cell_id == raw.cell_id)
                            .then(|| decision_from_stored(&stored.outcome))
                    })
                    .unwrap_or_else(|| {
                        TokenMiserDecision::Hide(
                            "retained; prior reducer decision was unavailable".to_string(),
                        )
                    });
                Arc::new(OutputState {
                    raw,
                    persistence: OnceCell::new_with(Some(true)),
                    decision: OnceCell::new_with(Some(stored_decision)),
                })
            });
        }
        Self {
            outputs: Arc::new(RwLock::new(outputs)),
            terminal_cells: Arc::new(Mutex::new(terminal_cells)),
            explicit_retrieval_cells: Arc::default(),
        }
    }

    pub(super) fn mark_explicit_retrieval(&self, cell_id: &str) {
        if let Ok(mut cells) = self.explicit_retrieval_cells.lock() {
            cells.insert(cell_id.to_string());
        }
    }

    pub(super) fn take_explicit_retrieval(&self, cell_id: &str) -> bool {
        self.explicit_retrieval_cells
            .lock()
            .is_ok_and(|mut cells| cells.remove(cell_id))
    }

    pub(super) async fn reduce_terminal(
        &self,
        session: &Arc<Session>,
        turn: &Arc<TurnContext>,
        context: &ReductionContext,
        terminal: TerminalOutput<'_>,
        config: &InProcessTokenMiserConfig,
    ) -> Vec<FunctionCallOutputContentItem> {
        let TerminalOutput {
            script_status,
            success,
            content_items,
            max_output_tokens,
        } = terminal;
        if !output_fits_storage_bound(
            [
                context.turn_id.as_str(),
                context.call_id.as_str(),
                context.cell_id.as_str(),
                script_status,
            ],
            &content_items,
            MAX_PERSISTED_OUTPUT_ITEMS,
            MAX_PERSISTED_OUTPUT_SOURCE_BYTES,
        ) {
            return receipt_output(TokenMiserReceipt::output_too_large());
        }
        let state = {
            let Ok(mut cells) = self.terminal_cells.lock() else {
                return receipt_output(TokenMiserReceipt::storage_unavailable());
            };
            let key = (
                context.turn_id.clone(),
                context.call_id.clone(),
                context.cell_id.clone(),
            );
            if let Some(state) = cells.get(&key) {
                Arc::clone(state)
            } else {
                let state = Arc::new(OutputState {
                    raw: Arc::new(TokenMiserOutput {
                        version: TOKEN_MISER_VERSION,
                        object_id: Uuid::new_v4().to_string(),
                        thread_id: session.thread_id,
                        turn_id: context.turn_id.clone(),
                        call_id: context.call_id.clone(),
                        cell_id: context.cell_id.clone(),
                        script_status: script_status.to_string(),
                        success,
                        content_items,
                    }),
                    persistence: OnceCell::new(),
                    decision: OnceCell::new(),
                });
                cells.insert(key, Arc::clone(&state));
                state
            }
        };
        let service = self.clone();
        let session = Arc::clone(session);
        let turn = Arc::clone(turn);
        let context = context.clone();
        let task_config = config.clone();
        let task_state = Arc::clone(&state);
        let resolution = tokio::spawn(async move {
            service
                .resolve_terminal(&session, &turn, &context, &task_config, &task_state)
                .await
        });
        let Ok(Some(decision)) = resolution.await else {
            return receipt_output(TokenMiserReceipt::storage_unavailable());
        };
        let receipt = TokenMiserReceipt::retained(
            &state.raw.object_id,
            match &decision {
                TokenMiserDecision::Passthrough => "passed through by Luna",
                TokenMiserDecision::Replace(_) => "reduced by Luna",
                TokenMiserDecision::Hide(reason) => reason.as_str(),
            },
        );
        let mut visible = match decision {
            TokenMiserDecision::Passthrough => {
                super::truncate_code_mode_result(state.raw.content_items.clone(), max_output_tokens)
            }
            TokenMiserDecision::Replace(replacement) => {
                let replacement = truncate_utf8_bytes(&replacement, config.max_replacement_bytes);
                let mut items = vec![
                    CodeModeOutputReplacementFence::opening().into_output_item(),
                    FunctionCallOutputContentItem::InputText { text: replacement },
                    CodeModeOutputReplacementFence::closing().into_output_item(),
                ];
                items = super::truncate_code_mode_result(items, max_output_tokens);
                items
            }
            TokenMiserDecision::Hide(_) => Vec::new(),
        };
        visible.extend(receipt_output(receipt));
        visible
    }

    async fn resolve_terminal(
        &self,
        session: &Arc<Session>,
        turn: &Arc<TurnContext>,
        context: &ReductionContext,
        config: &InProcessTokenMiserConfig,
        state: &Arc<OutputState>,
    ) -> Option<TokenMiserDecision> {
        let persisted = *state
            .persistence
            .get_or_init(|| async {
                let persisted = session
                    .persist_token_miser_output(Arc::clone(&state.raw))
                    .await;
                if persisted && let Ok(mut outputs) = self.outputs.write() {
                    outputs.insert(state.raw.object_id.clone(), Arc::clone(&state.raw));
                }
                persisted
            })
            .await;
        if !persisted {
            return None;
        }
        let decision = state
            .decision
            .get_or_init(|| async {
                let run = reducer::run(session, turn, context, &state.raw, config).await;
                let record = TokenMiserDecisionRecord {
                    version: TOKEN_MISER_VERSION,
                    object_id: state.raw.object_id.clone(),
                    thread_id: state.raw.thread_id,
                    turn_id: state.raw.turn_id.clone(),
                    call_id: state.raw.call_id.clone(),
                    cell_id: state.raw.cell_id.clone(),
                    outcome: decision_to_stored(&run.decision),
                    usage: run.usage,
                };
                if !session.commit_token_miser_decision(turn, record).await {
                    return TokenMiserDecision::Hide(
                        "retained; reducer decision storage was unavailable".to_string(),
                    );
                }
                run.decision
            })
            .await
            .clone();
        Some(decision)
    }

    pub(super) fn read(
        &self,
        thread_id: codex_protocol::ThreadId,
        object_id: &str,
        item_index: usize,
        offset: usize,
        requested_bytes: usize,
    ) -> Result<Value, String> {
        let output = self.lookup(thread_id, object_id)?;
        let item = output
            .content_items
            .get(item_index)
            .ok_or_else(|| format!("content item {item_index} does not exist"))?;
        let (kind, source, detail) = item_source(item);
        if offset > source.len() || !source.is_char_boundary(offset) {
            return Err("offset must be a UTF-8 boundary within the selected item".to_string());
        }
        let requested_bytes = requested_bytes.clamp(1, MAX_READ_BYTES);
        let mut end = offset.saturating_add(requested_bytes).min(source.len());
        while end > offset && !source.is_char_boundary(end) {
            end -= 1;
        }
        Ok(json!({
            "object_id": object_id,
            "item_index": item_index,
            "item_count": output.content_items.len(),
            "kind": kind,
            "detail": detail,
            "offset": offset,
            "content": &source[offset..end],
            "next_offset": (end < source.len()).then_some(end),
            "total_bytes": source.len(),
        }))
    }

    pub(super) fn search(
        &self,
        thread_id: codex_protocol::ThreadId,
        object_id: &str,
        query: &str,
        requested_results: usize,
    ) -> Result<Value, String> {
        if query.is_empty() || query.len() > MAX_SEARCH_QUERY_BYTES {
            return Err(format!(
                "query must contain 1..={MAX_SEARCH_QUERY_BYTES} UTF-8 bytes"
            ));
        }
        let output = self.lookup(thread_id, object_id)?;
        let max_results = requested_results.clamp(1, MAX_SEARCH_RESULTS);
        let mut matches = Vec::new();
        for (item_index, item) in output.content_items.iter().enumerate() {
            let FunctionCallOutputContentItem::InputText { text } = item else {
                continue;
            };
            for (line_index, line) in text.split_inclusive('\n').enumerate() {
                let Some(column) = line.find(query) else {
                    continue;
                };
                matches.push(json!({
                    "item_index": item_index,
                    "line": line_index + 1,
                    "column": column,
                    "snippet": truncate_utf8_bytes(line, MAX_SEARCH_SNIPPET_BYTES),
                }));
                if matches.len() == max_results {
                    break;
                }
            }
            if matches.len() == max_results {
                break;
            }
        }
        loop {
            let result = json!({
                "object_id": object_id,
                "query": query,
                "matches": &matches,
                "max_results": max_results,
            });
            if result.to_string().len() <= MAX_READ_BYTES || matches.pop().is_none() {
                return Ok(result);
            }
        }
    }

    fn lookup(
        &self,
        thread_id: codex_protocol::ThreadId,
        object_id: &str,
    ) -> Result<Arc<TokenMiserOutput>, String> {
        let output = self
            .outputs
            .read()
            .map_err(|_| "Token Miser output store is unavailable".to_string())?
            .get(object_id)
            .cloned()
            .ok_or_else(|| "Token Miser output was not found in this session".to_string())?;
        if output.thread_id != thread_id {
            return Err("Token Miser output was not found in this session".to_string());
        }
        Ok(output)
    }
}

fn terminal_key(output: &TokenMiserOutput) -> TerminalKey {
    (
        output.turn_id.clone(),
        output.call_id.clone(),
        output.cell_id.clone(),
    )
}

fn decision_to_stored(decision: &TokenMiserDecision) -> TokenMiserStoredOutcome {
    match decision {
        TokenMiserDecision::Passthrough => TokenMiserStoredOutcome::Passthrough,
        TokenMiserDecision::Replace(replacement) => TokenMiserStoredOutcome::Replace {
            replacement: replacement.clone(),
        },
        TokenMiserDecision::Hide(reason) => TokenMiserStoredOutcome::Hide {
            reason: reason.clone(),
        },
    }
}

fn decision_from_stored(outcome: &TokenMiserStoredOutcome) -> TokenMiserDecision {
    match outcome {
        TokenMiserStoredOutcome::Passthrough => TokenMiserDecision::Passthrough,
        TokenMiserStoredOutcome::Replace { replacement } => {
            TokenMiserDecision::Replace(replacement.clone())
        }
        TokenMiserStoredOutcome::Hide { reason } => TokenMiserDecision::Hide(reason.clone()),
    }
}

fn truncate_utf8_bytes(value: &str, max_bytes: usize) -> String {
    let mut end = value.len().min(max_bytes);
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

fn item_source(item: &FunctionCallOutputContentItem) -> (&'static str, &str, Option<Value>) {
    match item {
        FunctionCallOutputContentItem::InputText { text } => ("input_text", text, None),
        FunctionCallOutputContentItem::InputImage { image_url, detail } => {
            ("input_image", image_url, Some(json!(detail)))
        }
        FunctionCallOutputContentItem::InputAudio { audio_url } => ("input_audio", audio_url, None),
        FunctionCallOutputContentItem::EncryptedContent { encrypted_content } => {
            ("encrypted_content", encrypted_content, None)
        }
    }
}

fn output_fits_storage_bound<'a>(
    metadata: impl IntoIterator<Item = &'a str>,
    items: &[FunctionCallOutputContentItem],
    max_items: usize,
    max_source_bytes: usize,
) -> bool {
    let metadata_bytes = metadata
        .into_iter()
        .try_fold(0_usize, |total, value| total.checked_add(value.len()));
    items.len() <= max_items
        && metadata_bytes
            .and_then(|total| {
                items.iter().try_fold(total, |total, item| {
                    total.checked_add(item_source(item).1.len())
                })
            })
            .is_some_and(|total| total <= max_source_bytes)
}

fn receipt_output(receipt: TokenMiserReceipt) -> Vec<FunctionCallOutputContentItem> {
    vec![FunctionCallOutputContentItem::InputText {
        text: receipt.render(),
    }]
}

#[cfg(test)]
#[path = "token_miser_tests.rs"]
mod tests;
