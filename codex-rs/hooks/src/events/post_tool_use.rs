use std::path::PathBuf;

use codex_protocol::ThreadId;
use codex_protocol::protocol::HookCompletedEvent;
use codex_protocol::protocol::HookEventName;
use codex_protocol::protocol::HookOutputEntry;
use codex_protocol::protocol::HookOutputEntryKind;
use codex_protocol::protocol::HookRunStatus;
use codex_protocol::protocol::HookRunSummary;
use codex_utils_absolute_path::AbsolutePathBuf;
use serde_json::Value;

use super::common;
use crate::engine::ClaudeHooksEngine;
use crate::engine::ConfiguredHandler;
use crate::engine::HandlerRunResult;
use crate::engine::dispatcher;
use crate::engine::output_parser;
use crate::output_spill::AdditionalContext;
use crate::schema::PostToolUseCommandInput;
use crate::schema::SubagentCommandInputFields;

#[derive(Debug, Clone)]
pub struct PostToolUseRequest {
    pub session_id: ThreadId,
    pub turn_id: String,
    pub subagent: Option<common::SubagentHookContext>,
    pub cwd: AbsolutePathBuf,
    pub transcript_path: Option<PathBuf>,
    pub model: String,
    pub permission_mode: String,
    pub tool_name: String,
    pub matcher_aliases: Vec<String>,
    pub tool_use_id: String,
    pub tool_input: Value,
    pub tool_response: Value,
    pub exact_tool_response: Option<Value>,
    pub is_code_mode_nested: bool,
    pub code_mode_cell_id: Option<String>,
    pub code_mode_tool_call_id: Option<String>,
    pub parent_intent: Option<String>,
}

impl PostToolUseRequest {
    /// Serializes the stable command-hook stdin contract for a managed transport.
    pub fn command_input_json(&self) -> Result<String, serde_json::Error> {
        command_input_json(self)
    }
}

#[derive(Debug)]
pub struct PostToolUseOutcome {
    pub hook_events: Vec<HookCompletedEvent>,
    pub should_block: bool,
    pub additional_contexts: Vec<String>,
    pub feedback_message: Option<String>,
    /// Response identities whose hook feedback was selected as replacement text.
    pub replacement_response_ids: Vec<String>,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct PostToolUseHandlerData {
    should_block: bool,
    additional_contexts_for_model: Vec<AdditionalContext>,
    feedback_messages_for_model: Vec<String>,
    replacement_response_ids: Vec<String>,
}

pub(crate) fn preview(
    handlers: &[ConfiguredHandler],
    request: &PostToolUseRequest,
) -> Vec<HookRunSummary> {
    let matcher_inputs = common::matcher_inputs(&request.tool_name, &request.matcher_aliases);
    dispatcher::select_handlers_for_matcher_inputs(
        handlers,
        HookEventName::PostToolUse,
        &matcher_inputs,
    )
    .into_iter()
    .map(|handler| {
        common::hook_run_for_tool_use(dispatcher::running_summary(&handler), &request.tool_use_id)
    })
    .collect()
}

pub(crate) async fn run(
    engine: &ClaudeHooksEngine,
    request: PostToolUseRequest,
) -> PostToolUseOutcome {
    let matcher_inputs = common::matcher_inputs(&request.tool_name, &request.matcher_aliases);
    let matched = dispatcher::select_handlers_for_matcher_inputs(
        &engine.handlers,
        HookEventName::PostToolUse,
        &matcher_inputs,
    );
    if matched.is_empty() {
        return PostToolUseOutcome {
            hook_events: Vec::new(),
            should_block: false,
            additional_contexts: Vec::new(),
            feedback_message: None,
            replacement_response_ids: Vec::new(),
        };
    }

    let input_json = match command_input_json(&request) {
        Ok(input_json) => input_json,
        Err(error) => {
            let hook_events = common::serialization_failure_hook_events_for_tool_use(
                matched,
                Some(request.turn_id.clone()),
                format!("failed to serialize post tool use hook input: {error}"),
                &request.tool_use_id,
            );
            return serialization_failure_outcome(hook_events);
        }
    };

    let results = dispatcher::execute_handlers(
        engine,
        matched,
        input_json,
        request.cwd.as_path(),
        Some(request.turn_id.clone()),
        parse_completed,
    )
    .await;

    let additional_contexts = common::flatten_additional_contexts(
        results
            .iter()
            .map(|result| result.data.additional_contexts_for_model.as_slice()),
    );
    let additional_contexts = engine
        .command_runtime
        .output_spiller()
        .maybe_spill_additional_contexts(additional_contexts)
        .await;
    let should_block = results.iter().any(|result| result.data.should_block);
    let feedback_message = common::join_text_chunks(
        results
            .iter()
            .flat_map(|result| result.data.feedback_messages_for_model.clone())
            .collect(),
    );
    let replacement_response_ids = results
        .iter()
        .flat_map(|result| result.data.replacement_response_ids.iter().cloned())
        .fold(Vec::new(), |mut response_ids, response_id| {
            if !response_ids.contains(&response_id) {
                response_ids.push(response_id);
            }
            response_ids
        });

    PostToolUseOutcome {
        hook_events: results
            .into_iter()
            .map(|result| {
                common::hook_completed_for_tool_use(result.completed, &request.tool_use_id)
            })
            .collect(),
        should_block,
        additional_contexts,
        feedback_message,
        replacement_response_ids,
    }
}

/// Serializes command stdin for a selected `PostToolUse` hook.
///
/// Handler selection may include internal matcher aliases, but hook stdin keeps
/// the canonical `tool_name` for logs and for consumers that pair pre/post
/// events across processes. Shell-like tools pass `{ "command": ... }` as
/// `tool_input`; MCP tools pass their resolved JSON arguments.
fn command_input_json(request: &PostToolUseRequest) -> Result<String, serde_json::Error> {
    let subagent = SubagentCommandInputFields::from(request.subagent.as_ref());
    serde_json::to_string(&PostToolUseCommandInput {
        session_id: request.session_id.to_string(),
        turn_id: request.turn_id.clone(),
        agent_id: subagent.agent_id,
        agent_type: subagent.agent_type,
        transcript_path: crate::schema::NullableString::from_path(request.transcript_path.clone()),
        cwd: request.cwd.display().to_string(),
        hook_event_name: "PostToolUse".to_string(),
        model: request.model.clone(),
        permission_mode: request.permission_mode.clone(),
        tool_name: request.tool_name.clone(),
        tool_input: request.tool_input.clone(),
        tool_response: request.tool_response.clone(),
        token_miser_exact_tool_response_version: request.exact_tool_response.as_ref().map(|_| 1),
        token_miser_exact_tool_response: request.exact_tool_response.clone(),
        tool_use_id: request.tool_use_id.clone(),
        is_code_mode_nested: request.is_code_mode_nested,
        code_mode_cell_id: request.code_mode_cell_id.clone(),
        code_mode_tool_call_id: request.code_mode_tool_call_id.clone(),
        parent_intent: request.parent_intent.clone(),
        token_miser_acceptance_version: 1,
        token_miser_grouping_version: 1,
    })
}

fn parse_completed(
    handler: &ConfiguredHandler,
    run_result: HandlerRunResult,
    turn_id: Option<String>,
) -> dispatcher::ParsedHandler<PostToolUseHandlerData> {
    let mut entries = Vec::new();
    let mut status = HookRunStatus::Completed;
    let mut should_block = false;
    let mut additional_contexts_for_model = Vec::new();
    let mut feedback_messages_for_model = Vec::new();
    let mut replacement_response_ids = Vec::new();

    match run_result.error.as_deref() {
        Some(error) => {
            status = HookRunStatus::Failed;
            entries.push(HookOutputEntry {
                kind: HookOutputEntryKind::Error,
                text: error.to_string(),
            });
        }
        None => match run_result.exit_code {
            Some(0) => {
                let trimmed_stdout = run_result.stdout.trim();
                if trimmed_stdout.is_empty() {
                } else if let Some(parsed) = output_parser::parse_post_tool_use(&run_result.stdout)
                {
                    let response_id = parsed
                        .response_id
                        .as_deref()
                        .map(str::trim)
                        .filter(|response_id| !response_id.is_empty())
                        .map(str::to_string);
                    if let Some(system_message) = parsed.universal.system_message {
                        entries.push(HookOutputEntry {
                            kind: HookOutputEntryKind::Warning,
                            text: system_message,
                        });
                    }
                    if (!handler.can_apply_control_effects()
                        || parsed.invalid_reason.is_none() && parsed.invalid_block_reason.is_none())
                        && let Some(additional_context) = parsed.additional_context
                    {
                        common::append_additional_context(
                            &mut entries,
                            &mut additional_contexts_for_model,
                            handler,
                            additional_context,
                        );
                    }
                    if handler.can_apply_control_effects() {
                        if !parsed.universal.continue_processing {
                            status = HookRunStatus::Stopped;
                            let stop_text = parsed.universal.stop_reason.unwrap_or_else(|| {
                                "PostToolUse hook stopped execution".to_string()
                            });
                            entries.push(HookOutputEntry {
                                kind: HookOutputEntryKind::Stop,
                                text: stop_text.clone(),
                            });
                            let model_feedback = parsed
                                .reason
                                .as_deref()
                                .and_then(common::trimmed_non_empty)
                                .unwrap_or(stop_text);
                            feedback_messages_for_model.push(model_feedback);
                            if let Some(response_id) = response_id {
                                replacement_response_ids.push(response_id);
                            }
                        } else if let Some(invalid_reason) = parsed.invalid_reason {
                            status = HookRunStatus::Failed;
                            entries.push(HookOutputEntry {
                                kind: HookOutputEntryKind::Error,
                                text: invalid_reason,
                            });
                        } else if let Some(invalid_block_reason) = parsed.invalid_block_reason {
                            status = HookRunStatus::Failed;
                            entries.push(HookOutputEntry {
                                kind: HookOutputEntryKind::Error,
                                text: invalid_block_reason,
                            });
                        } else if parsed.should_block {
                            status = HookRunStatus::Blocked;
                            should_block = true;
                            if let Some(reason) = parsed.reason {
                                entries.push(HookOutputEntry {
                                    kind: HookOutputEntryKind::Feedback,
                                    text: reason.clone(),
                                });
                                feedback_messages_for_model.push(reason);
                                if let Some(response_id) = response_id {
                                    replacement_response_ids.push(response_id);
                                }
                            }
                        }
                    }
                } else if output_parser::looks_like_json(&run_result.stdout) {
                    status = HookRunStatus::Failed;
                    entries.push(HookOutputEntry {
                        kind: HookOutputEntryKind::Error,
                        text: "hook returned invalid post-tool-use JSON output".to_string(),
                    });
                }
            }
            Some(2) if handler.can_apply_control_effects() => {
                if let Some(reason) = common::trimmed_non_empty(&run_result.stderr) {
                    status = HookRunStatus::Blocked;
                    should_block = true;
                    entries.push(HookOutputEntry {
                        kind: HookOutputEntryKind::Feedback,
                        text: reason.clone(),
                    });
                    feedback_messages_for_model.push(reason);
                } else {
                    status = HookRunStatus::Failed;
                    entries.push(HookOutputEntry {
                        kind: HookOutputEntryKind::Error,
                        text: "PostToolUse hook exited with code 2 but did not write feedback to stderr".to_string(),
                    });
                }
            }
            Some(exit_code) => {
                status = HookRunStatus::Failed;
                entries.push(HookOutputEntry {
                    kind: HookOutputEntryKind::Error,
                    text: format!("hook exited with code {exit_code}"),
                });
            }
            None => {
                status = HookRunStatus::Failed;
                entries.push(HookOutputEntry {
                    kind: HookOutputEntryKind::Error,
                    text: "hook exited without a status code".to_string(),
                });
            }
        },
    }

    let completed = HookCompletedEvent {
        turn_id,
        run: dispatcher::completed_summary(handler, &run_result, status, entries),
    };

    dispatcher::ParsedHandler {
        completed,
        data: PostToolUseHandlerData {
            should_block,
            additional_contexts_for_model,
            feedback_messages_for_model,
            replacement_response_ids,
        },
        completion_order: 0,
    }
}

fn serialization_failure_outcome(hook_events: Vec<HookCompletedEvent>) -> PostToolUseOutcome {
    PostToolUseOutcome {
        hook_events,
        should_block: false,
        additional_contexts: Vec::new(),
        feedback_message: None,
        replacement_response_ids: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use codex_protocol::ThreadId;
    use codex_protocol::protocol::HookEventName;
    use codex_protocol::protocol::HookOutputEntry;
    use codex_protocol::protocol::HookOutputEntryKind;
    use codex_protocol::protocol::HookRunStatus;
    use codex_utils_absolute_path::test_support::PathBufExt;
    use codex_utils_absolute_path::test_support::test_path_buf;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::PostToolUseHandlerData;
    use super::command_input_json;
    use super::parse_completed;
    use super::preview;
    use crate::engine::ConfiguredHandler;
    use crate::engine::HandlerRunResult;
    use crate::events::common;
    use crate::output_spill::AdditionalContext;
    use crate::output_spill::AdditionalContextLimit;

    #[test]
    fn command_input_uses_request_tool_name() {
        let mut request = request_for_tool_use("call-apply-patch");
        request.tool_name = "apply_patch".to_string();

        let input_json = command_input_json(&request).expect("serialize command input");
        let input: serde_json::Value =
            serde_json::from_str(&input_json).expect("parse command input");

        assert_eq!(input["tool_name"], "apply_patch");
    }

    #[test]
    fn block_decision_stops_normal_processing() {
        let parsed = parse_completed(
            &handler(),
            run_result(
                Some(0),
                r#"{"decision":"block","reason":"bash output looked sketchy","hookSpecificOutput":{"hookEventName":"PostToolUse","response_id":"block-gate-1"}}"#,
                "",
            ),
            Some("turn-1".to_string()),
        );

        assert_eq!(
            parsed.data,
            PostToolUseHandlerData {
                should_block: true,
                additional_contexts_for_model: Vec::new(),
                feedback_messages_for_model: vec!["bash output looked sketchy".to_string()],
                replacement_response_ids: vec!["block-gate-1".to_string()],
            }
        );
        assert_eq!(parsed.completed.run.status, HookRunStatus::Blocked);
    }

    #[test]
    fn additional_context_is_recorded() {
        let mut handler = handler();
        handler.additional_context_limit = AdditionalContextLimit::from_config(Some(17));
        let parsed = parse_completed(
            &handler,
            run_result(
                Some(0),
                r#"{"hookSpecificOutput":{"hookEventName":"PostToolUse","additionalContext":"Remember the bash cleanup note."}}"#,
                "",
            ),
            Some("turn-1".to_string()),
        );

        assert_eq!(
            parsed.data,
            PostToolUseHandlerData {
                should_block: false,
                additional_contexts_for_model: vec![AdditionalContext {
                    text: "Remember the bash cleanup note.".to_string(),
                    limit: AdditionalContextLimit::from_config(Some(17)),
                }],
                feedback_messages_for_model: Vec::new(),
                replacement_response_ids: Vec::new(),
            }
        );
        assert_eq!(
            parsed.completed.run.entries,
            vec![HookOutputEntry {
                kind: HookOutputEntryKind::Context,
                text: "Remember the bash cleanup note.".to_string(),
            }]
        );
    }

    #[test]
    fn unsupported_updated_mcp_tool_output_fails_open() {
        let stdout = r#"{"hookSpecificOutput":{"hookEventName":"PostToolUse","updatedMCPToolOutput":{"ok":true},"additionalContext":"preserved"}}"#;
        let parsed = parse_completed(
            &handler(),
            run_result(Some(0), stdout, ""),
            Some("turn-1".to_string()),
        );

        assert_eq!(
            parsed.data,
            PostToolUseHandlerData {
                should_block: false,
                additional_contexts_for_model: Vec::new(),
                feedback_messages_for_model: Vec::new(),
                replacement_response_ids: Vec::new(),
            }
        );
        assert_eq!(parsed.completed.run.status, HookRunStatus::Failed);
        assert_eq!(
            parsed.completed.run.entries,
            vec![HookOutputEntry {
                kind: HookOutputEntryKind::Error,
                text: "PostToolUse hook returned unsupported updatedMCPToolOutput".to_string(),
            }]
        );

        let async_handler = handler_with_async(/*async*/ true);
        let parsed = parse_completed(
            &async_handler,
            run_result(Some(0), stdout, ""),
            Some("turn-1".to_string()),
        );
        assert_eq!(parsed.completed.run.status, HookRunStatus::Completed);
        assert_eq!(
            parsed.completed.run.entries,
            vec![HookOutputEntry {
                kind: HookOutputEntryKind::Context,
                text: "preserved".to_string(),
            }],
        );
    }

    #[test]
    fn exit_two_blocks_with_feedback() {
        let parsed = parse_completed(
            &handler(),
            run_result(Some(2), "", "post hook says pause"),
            Some("turn-1".to_string()),
        );

        assert_eq!(
            parsed.data,
            PostToolUseHandlerData {
                should_block: true,
                additional_contexts_for_model: Vec::new(),
                feedback_messages_for_model: vec!["post hook says pause".to_string()],
                replacement_response_ids: Vec::new(),
            }
        );
        assert_eq!(parsed.completed.run.status, HookRunStatus::Blocked);
    }

    #[test]
    fn continue_false_stops_with_reason() {
        let parsed = parse_completed(
            &handler(),
            run_result(
                Some(0),
                r#"{"continue":false,"stopReason":"halt after bash output","reason":"post-tool hook says stop","hookSpecificOutput":{"hookEventName":"PostToolUse","response_id":"gate-direct-1"}}"#,
                "",
            ),
            Some("turn-1".to_string()),
        );

        assert_eq!(
            parsed.data,
            PostToolUseHandlerData {
                should_block: false,
                additional_contexts_for_model: Vec::new(),
                feedback_messages_for_model: vec!["post-tool hook says stop".to_string()],
                replacement_response_ids: vec!["gate-direct-1".to_string()],
            }
        );
        assert_eq!(parsed.completed.run.status, HookRunStatus::Stopped);
        assert_eq!(
            parsed.completed.run.entries,
            vec![HookOutputEntry {
                kind: HookOutputEntryKind::Stop,
                text: "halt after bash output".to_string(),
            }]
        );
    }

    #[test]
    fn continue_false_without_reason_synthesizes_feedback() {
        let parsed = parse_completed(
            &handler(),
            run_result(Some(0), r#"{"continue":false}"#, ""),
            Some("turn-1".to_string()),
        );

        assert_eq!(
            parsed.data,
            PostToolUseHandlerData {
                should_block: false,
                additional_contexts_for_model: Vec::new(),
                feedback_messages_for_model: vec!["PostToolUse hook stopped execution".to_string()],
                replacement_response_ids: Vec::new(),
            }
        );
        assert_eq!(parsed.completed.run.status, HookRunStatus::Stopped);
        assert_eq!(
            parsed.completed.run.entries,
            vec![HookOutputEntry {
                kind: HookOutputEntryKind::Stop,
                text: "PostToolUse hook stopped execution".to_string(),
            }]
        );
    }

    #[test]
    fn plain_stdout_is_ignored_for_post_tool_use() {
        let parsed = parse_completed(
            &handler(),
            run_result(Some(0), "plain text only", ""),
            Some("turn-1".to_string()),
        );

        assert_eq!(
            parsed.data,
            PostToolUseHandlerData {
                should_block: false,
                additional_contexts_for_model: Vec::new(),
                feedback_messages_for_model: Vec::new(),
                replacement_response_ids: Vec::new(),
            }
        );
        assert_eq!(parsed.completed.run.status, HookRunStatus::Completed);
        assert_eq!(parsed.completed.run.entries, Vec::<HookOutputEntry>::new());
    }

    #[test]
    fn preview_and_completed_run_ids_include_tool_use_id() {
        let request = request_for_tool_use("tool-call-456");
        let runs = preview(&[handler()], &request);

        assert_eq!(runs.len(), 1);
        assert_eq!(
            runs[0].id,
            format!(
                "post-tool-use:0:{}:tool-call-456",
                test_path_buf("/tmp/hooks.json").display()
            )
        );

        let parsed = parse_completed(
            &handler(),
            run_result(Some(0), "", ""),
            Some("turn-1".to_string()),
        );
        let completed = common::hook_completed_for_tool_use(parsed.completed, &request.tool_use_id);

        assert_eq!(completed.run.id, runs[0].id);
    }

    #[test]
    fn serialization_failure_run_ids_include_tool_use_id() {
        let request = request_for_tool_use("tool-call-456");
        let runs = preview(&[handler()], &request);

        let completed = common::serialization_failure_hook_events_for_tool_use(
            vec![handler()],
            Some(request.turn_id.clone()),
            "serialize failed".into(),
            &request.tool_use_id,
        );

        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].run.id, runs[0].id);
    }

    fn handler() -> ConfiguredHandler {
        handler_with_async(/*async*/ false)
    }

    fn handler_with_async(r#async: bool) -> ConfiguredHandler {
        ConfiguredHandler {
            event_name: HookEventName::PostToolUse,
            matcher: Some("^Bash$".to_string()),
            timeout_sec: 5,
            status_message: Some("running post tool use hook".to_string()),
            additional_context_limit: Default::default(),
            source_path: test_path_buf("/tmp/hooks.json").abs().into(),
            source: codex_protocol::protocol::HookSource::User,
            display_order: 0,
            kind: crate::engine::ConfiguredHandlerKind::Command {
                command: "python3 post_tool_use_hook.py".to_string(),
                r#async,
                env: std::collections::HashMap::new(),
            },
        }
    }

    fn run_result(exit_code: Option<i32>, stdout: &str, stderr: &str) -> HandlerRunResult {
        HandlerRunResult {
            started_at: 1_700_000_000,
            completed_at: 1_700_000_001,
            duration_ms: 12,
            exit_code,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            error: None,
        }
    }

    fn request_for_tool_use(tool_use_id: &str) -> super::PostToolUseRequest {
        super::PostToolUseRequest {
            session_id: ThreadId::new(),
            turn_id: "turn-1".to_string(),
            subagent: None,
            cwd: test_path_buf("/tmp").abs(),
            transcript_path: None,
            model: "gpt-test".to_string(),
            permission_mode: "default".to_string(),
            tool_name: "Bash".to_string(),
            matcher_aliases: Vec::new(),
            tool_use_id: tool_use_id.to_string(),
            tool_input: json!({ "command": "echo hello" }),
            tool_response: json!({"ok": true}),
            exact_tool_response: None,
            is_code_mode_nested: false,
            code_mode_cell_id: None,
            code_mode_tool_call_id: None,
            parent_intent: None,
        }
    }

    #[test]
    fn command_input_marks_only_code_mode_nested_tool_calls() {
        let direct = request_for_tool_use("tool-1");
        let direct: serde_json::Value = serde_json::from_str(
            &super::command_input_json(&direct).expect("serialize direct hook input"),
        )
        .expect("parse direct hook input");
        assert_eq!(direct.get("is_code_mode_nested"), Some(&json!(false)));
        assert_eq!(direct["token_miser_acceptance_version"], 1);
        assert_eq!(direct["token_miser_grouping_version"], 1);
        assert_eq!(direct.get("code_mode_cell_id"), None);
        assert_eq!(direct.get("code_mode_tool_call_id"), None);
        assert_eq!(direct.get("parent_intent"), None);

        let mut nested = request_for_tool_use("tool-2");
        nested.is_code_mode_nested = true;
        nested.code_mode_cell_id = Some("cell-1".to_string());
        nested.code_mode_tool_call_id = Some("nested-1".to_string());
        nested.parent_intent = Some("Inspect the selected nested output.".to_string());
        let nested: serde_json::Value = serde_json::from_str(
            &super::command_input_json(&nested).expect("serialize nested hook input"),
        )
        .expect("parse nested hook input");
        assert_eq!(nested.get("is_code_mode_nested"), Some(&json!(true)));
        assert_eq!(nested["token_miser_acceptance_version"], 1);
        assert_eq!(nested["token_miser_grouping_version"], 1);
        assert_eq!(nested["code_mode_cell_id"], "cell-1");
        assert_eq!(nested["code_mode_tool_call_id"], "nested-1");
        assert_eq!(
            nested["parent_intent"],
            "Inspect the selected nested output."
        );
    }
}
