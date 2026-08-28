//! Authoritative continuation state produced by nested unified-exec calls.
//!
//! Reducers may replace stdout and stderr, but they must not become the authority
//! for handles the parent model needs to continue a running process. This module
//! derives that state from Codex-owned nested-tool results and gives the reducer
//! boundary a bounded, structured envelope to preserve independently.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Mutex;

use codex_code_mode::CellId;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_tools::ToolName;
use serde::Serialize;
use serde_json::Value as JsonValue;

use crate::context::CodeModeActionableStateFragment;
use crate::unified_exec::MAX_UNIFIED_EXEC_PROCESSES;

pub(super) const ACTIONABLE_STATE_VERSION: u32 = 1;

const EXEC_COMMAND_TOOL_NAME: &str = "exec_command";
const WRITE_STDIN_TOOL_NAME: &str = "write_stdin";

#[derive(Default)]
pub(super) struct ActionableStateStore {
    cells: Mutex<HashMap<CellId, BTreeMap<i32, ActionableStateEntry>>>,
}

impl ActionableStateStore {
    pub(super) fn record(
        &self,
        cell_id: &CellId,
        tool_name: &ToolName,
        input: Option<&JsonValue>,
        result: &JsonValue,
    ) {
        let Some(entry) = from_nested_tool_result(tool_name, input, result) else {
            return;
        };
        if let Ok(mut cells) = self.cells.lock() {
            cells
                .entry(cell_id.clone())
                .or_default()
                .insert(entry.process_id, entry);
        }
    }

    pub(super) fn read(&self, cell_id: &CellId, take: bool) -> Option<ActionableState> {
        let mut cells = self.cells.lock().ok()?;
        let entries = if take {
            cells.remove(cell_id)?
        } else {
            cells.get(cell_id)?.clone()
        };
        ActionableState::new(entries.into_values().collect())
    }

    pub(super) fn clear(&self) {
        if let Ok(mut cells) = self.cells.lock() {
            cells.clear();
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct ActionableState {
    pub version: u32,
    pub entries: Vec<ActionableStateEntry>,
}

impl ActionableState {
    pub(super) fn new(mut entries: Vec<ActionableStateEntry>) -> Option<Self> {
        entries.sort_by_key(|entry| entry.process_id);
        entries.dedup_by_key(|entry| entry.process_id);
        if entries.is_empty() {
            return None;
        }
        // Unified exec cannot have more live handles than this. If one cell
        // observes more terminal sessions, discard only bounded terminal
        // observations; running entries sort first so no pollable handle is
        // discarded.
        if entries.len() > MAX_UNIFIED_EXEC_PROCESSES {
            entries.sort_by_key(|entry| {
                (
                    entry.state == ActionableStateStatus::Completed,
                    entry.process_id,
                )
            });
            entries.truncate(MAX_UNIFIED_EXEC_PROCESSES);
            entries.sort_by_key(|entry| entry.process_id);
        }
        Some(Self {
            version: ACTIONABLE_STATE_VERSION,
            entries,
        })
    }

    pub(super) fn output_items(&self) -> Option<Vec<FunctionCallOutputContentItem>> {
        self.entries
            .iter()
            .map(|entry| {
                let fragment = match entry.state {
                    ActionableStateStatus::Running => CodeModeActionableStateFragment::running(
                        self.version,
                        entry.session_id,
                        entry.process_id,
                        &entry.chunk_id,
                    ),
                    ActionableStateStatus::Completed => {
                        let exit_code = entry.exit_code?;
                        CodeModeActionableStateFragment::completed(
                            self.version,
                            entry.session_id,
                            entry.process_id,
                            &entry.chunk_id,
                            exit_code,
                        )
                    }
                }?;
                Some(fragment.into_output_item())
            })
            .collect()
    }

    pub(super) fn to_json_value(&self) -> JsonValue {
        serde_json::json!({
            "version": self.version,
            "entries": &self.entries,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct ActionableStateEntry {
    pub session_id: i32,
    pub process_id: i32,
    pub chunk_id: String,
    pub state: ActionableStateStatus,
    pub exit_code: Option<i32>,
    pub required_follow_up: Option<RequiredFollowUp>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ActionableStateStatus {
    Running,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct RequiredFollowUp {
    pub operation: String,
    pub arguments: WriteStdinFollowUpArguments,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct WriteStdinFollowUpArguments {
    pub session_id: i32,
    pub chars: String,
}

pub(super) fn from_nested_tool_result(
    tool_name: &ToolName,
    input: Option<&JsonValue>,
    result: &JsonValue,
) -> Option<ActionableStateEntry> {
    if !tool_name.is_default_namespace() {
        return None;
    }
    let fields = result.as_object()?;
    let chunk_id = fields.get("chunk_id")?.as_str()?.to_string();
    if chunk_id.len() != 6 || !chunk_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let result_session_id = fields
        .get("session_id")
        .and_then(JsonValue::as_i64)
        .and_then(|value| i32::try_from(value).ok());
    let exit_code = fields
        .get("exit_code")
        .and_then(JsonValue::as_i64)
        .and_then(|value| i32::try_from(value).ok());

    let (session_id, state) = match tool_name.name.as_str() {
        EXEC_COMMAND_TOOL_NAME => (result_session_id?, ActionableStateStatus::Running),
        WRITE_STDIN_TOOL_NAME => {
            let input_session_id = input?
                .get("session_id")?
                .as_i64()
                .and_then(|value| i32::try_from(value).ok())?;
            match result_session_id {
                Some(session_id) => (session_id, ActionableStateStatus::Running),
                None if exit_code.is_some() => (input_session_id, ActionableStateStatus::Completed),
                None => return None,
            }
        }
        _ => return None,
    };
    let required_follow_up = (state == ActionableStateStatus::Running).then(|| RequiredFollowUp {
        operation: WRITE_STDIN_TOOL_NAME.to_string(),
        arguments: WriteStdinFollowUpArguments {
            session_id,
            chars: String::new(),
        },
    });

    Some(ActionableStateEntry {
        session_id,
        process_id: session_id,
        chunk_id,
        state,
        exit_code,
        required_follow_up,
    })
}

#[cfg(test)]
#[path = "actionable_state_tests.rs"]
mod tests;
