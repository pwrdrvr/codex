use codex_app_server_protocol::CodeModeOutputReducerAcceptanceCapability;
use codex_app_server_protocol::CodeModeOutputReducerCapability;
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
            protocol_version: 2,
            config_key: "features.code_mode.output_reducer".to_string(),
            max_output_tokens_ceiling_config_key: "features.code_mode.max_output_tokens_ceiling"
                .to_string(),
            post_tool_use_nested_context_field: "is_code_mode_nested".to_string(),
            supports_thread_resume_overrides: true,
            acceptance: CodeModeOutputReducerAcceptanceCapability {
                descriptor_url_field: "acceptance_url".to_string(),
                response_id_field: "response_id".to_string(),
                callback_version: 2,
            },
        },
    }
}
