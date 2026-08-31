use codex_app_server_protocol::CodeModeActionableStateCapability;
use codex_app_server_protocol::CodeModeDeferredCompletionCapability;
use codex_app_server_protocol::CodeModeOutputReducerAcceptanceCapability;
use codex_app_server_protocol::CodeModeOutputReducerCapability;
use codex_app_server_protocol::CodeModeOutputReducerModelGuidanceCapability;
use codex_app_server_protocol::CodeModePostToolUseExactOutputCapability;
use codex_app_server_protocol::CodeModePostToolUseGroupingCapability;
use codex_app_server_protocol::DirectPostToolUseAcceptanceCapability;
use codex_app_server_protocol::PwrdrvrTokenMiserCapability;
use codex_app_server_protocol::ServerCapabilitiesReadResponse;
use codex_app_server_protocol::ServerDiagnosticsGauge;
use codex_app_server_protocol::ServerDiagnosticsProcess;
use codex_app_server_protocol::ServerDiagnosticsResponse;

pub(crate) fn read_server_diagnostics() -> ServerDiagnosticsResponse {
    let diagnostics = codex_diagnostics::snapshot();

    ServerDiagnosticsResponse {
        process: ServerDiagnosticsProcess {
            id: diagnostics.process.id,
            resident_memory_bytes: diagnostics.process.resident_memory_bytes,
            physical_footprint_bytes: diagnostics.process.physical_footprint_bytes,
        },
        gauges: diagnostics
            .gauges
            .into_iter()
            .map(|gauge| ServerDiagnosticsGauge {
                name: gauge.name.to_string(),
                value: gauge.value,
            })
            .collect(),
    }
}

pub(crate) fn read_server_capabilities() -> ServerCapabilitiesReadResponse {
    ServerCapabilitiesReadResponse {
        code_mode_output_reducer: CodeModeOutputReducerCapability {
            protocol_version: 1,
            continuation_guidance_version: 1,
            intent_context_version: 1,
            reducer_request_field: "parent_intent".to_string(),
            post_tool_use_field: "parent_intent".to_string(),
            model_guidance: CodeModeOutputReducerModelGuidanceCapability {
                version: 1,
                tool_description_config_key:
                    "features.code_mode.output_reducer.tool_description_guidance".to_string(),
                continuation_config_key: "features.code_mode.output_reducer.continuation_guidance"
                    .to_string(),
                model_visible_overhead_request_field: "model_visible_overhead_characters"
                    .to_string(),
            },
            actionable_state: CodeModeActionableStateCapability {
                version: 1,
                reducer_request_field: "actionable_state".to_string(),
                reducer_response_field: "actionable_state".to_string(),
                model_output_tag: "codex_actionable_state".to_string(),
            },
            deferred_completion: CodeModeDeferredCompletionCapability {
                version: 1,
                terminal_only: true,
                preserves_original_call_id: true,
                preserves_cell_id: true,
                wait_tool_name: "wait".to_string(),
            },
            config_key: "features.code_mode.output_reducer".to_string(),
            max_output_tokens_ceiling_config_key: "features.code_mode.max_output_tokens_ceiling"
                .to_string(),
            post_tool_use_nested_context_field: "is_code_mode_nested".to_string(),
            post_tool_use_grouping: CodeModePostToolUseGroupingCapability {
                version_field: "token_miser_grouping_version".to_string(),
                version: 1,
                cell_id_field: "code_mode_cell_id".to_string(),
                tool_call_id_field: "code_mode_tool_call_id".to_string(),
            },
            post_tool_use_exact_output: CodeModePostToolUseExactOutputCapability {
                version: 1,
                version_field: "token_miser_exact_tool_response_version".to_string(),
                response_field: "token_miser_exact_tool_response".to_string(),
            },
            supports_thread_resume_overrides: true,
            dynamic_tools_resume_field: "dynamicTools".to_string(),
            acceptance: CodeModeOutputReducerAcceptanceCapability {
                descriptor_url_field: "acceptance_url".to_string(),
                response_id_field: "response_id".to_string(),
                callback_version: 1,
                direct_post_tool_use: DirectPostToolUseAcceptanceCapability {
                    hook_response_id_field: "hookSpecificOutput.response_id".to_string(),
                    hook_acceptance_version_field: "token_miser_acceptance_version".to_string(),
                    hook_acceptance_version: 1,
                    session_id_field: "session_id".to_string(),
                    turn_id_field: "turn_id".to_string(),
                    tool_use_id_field: "tool_use_id".to_string(),
                },
            },
        },
        pwrdrvr_token_miser: PwrdrvrTokenMiserCapability {
            version: 1,
            identity: "pwrdrvr.pwragent.token-miser".to_string(),
            initialize_capability_field: "pwrdrvrTokenMiser".to_string(),
            thread_start_field: "pwrdrvrTokenMiser".to_string(),
            thread_resume_field: "pwrdrvrTokenMiser".to_string(),
            descriptor_environment_variable: "PWRAGENT_TOKEN_MISER_BRIDGE_DESCRIPTOR_PATH"
                .to_string(),
            descriptor_version: 1,
            code_mode_nested_post_tool_use: false,
        },
    }
}
