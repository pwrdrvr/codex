use std::time::Duration;

use anyhow::Result;
use app_test_support::DEFAULT_CLIENT_NAME;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use app_test_support::create_mock_responses_server_repeating_assistant;
use codex_app_server_protocol::ClientInfo;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::CodeModeActionableStateCapability;
use codex_app_server_protocol::CodeModeOutputReducerAcceptanceCapability;
use codex_app_server_protocol::CodeModeOutputReducerCapability;
use codex_app_server_protocol::CodeModeOutputReducerModelGuidanceCapability;
use codex_app_server_protocol::CodeModePostToolUseExactOutputCapability;
use codex_app_server_protocol::CodeModePostToolUseGroupingCapability;
use codex_app_server_protocol::DirectPostToolUseAcceptanceCapability;
use codex_app_server_protocol::InitializeCapabilities;
use codex_app_server_protocol::JSONRPCMessage;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ServerCapabilitiesReadParams;
use codex_app_server_protocol::ServerCapabilitiesReadResponse;
use codex_app_server_protocol::ServerDiagnosticsGauge;
use codex_app_server_protocol::ServerDiagnosticsParams;
use codex_app_server_protocol::ServerDiagnosticsResponse;
use codex_app_server_protocol::ThreadStartParams;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::timeout;

const READ_TIMEOUT: Duration = Duration::from_secs(20);

#[tokio::test]
async fn server_capabilities_advertises_code_mode_output_reducer_contract() -> Result<()> {
    let codex_home = TempDir::new()?;
    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build_initialized_with_timeout(READ_TIMEOUT)
        .await?;

    let capabilities: ServerCapabilitiesReadResponse = app_server
        .request(|request_id| ClientRequest::ServerCapabilitiesRead {
            request_id,
            params: ServerCapabilitiesReadParams::default(),
        })
        .await?;

    let capabilities_json = serde_json::to_value(&capabilities)?;
    assert_eq!(
        capabilities_json["codeModeOutputReducer"]["modelGuidance"],
        json!({
            "version": 1,
            "toolDescriptionConfigKey":
                "features.code_mode.output_reducer.tool_description_guidance",
            "continuationConfigKey":
                "features.code_mode.output_reducer.continuation_guidance",
            "modelVisibleOverheadRequestField": "model_visible_overhead_characters",
        })
    );

    assert_eq!(
        capabilities,
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
                    continuation_config_key:
                        "features.code_mode.output_reducer.continuation_guidance".to_string(),
                    model_visible_overhead_request_field: "model_visible_overhead_characters"
                        .to_string(),
                },
                actionable_state: CodeModeActionableStateCapability {
                    version: 1,
                    reducer_request_field: "actionable_state".to_string(),
                    reducer_response_field: "actionable_state".to_string(),
                    model_output_tag: "codex_actionable_state".to_string(),
                },
                config_key: "features.code_mode.output_reducer".to_string(),
                max_output_tokens_ceiling_config_key:
                    "features.code_mode.max_output_tokens_ceiling".to_string(),
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
        }
    );

    Ok(())
}

#[tokio::test]
async fn server_diagnostics_exposes_process_and_registered_thread_gauge() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri()).write(codex_home.path())?;
    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;
    app_server
        .start_thread(ThreadStartParams::default())
        .await?;

    let diagnostics: ServerDiagnosticsResponse = app_server
        .request(|request_id| ClientRequest::ServerDiagnostics {
            request_id,
            params: ServerDiagnosticsParams::default(),
        })
        .await?;

    assert!(diagnostics.process.id > 0);
    assert!(diagnostics.process.resident_memory_bytes.is_some());
    #[cfg(target_os = "macos")]
    assert!(diagnostics.process.physical_footprint_bytes.is_some());
    #[cfg(not(target_os = "macos"))]
    assert_eq!(diagnostics.process.physical_footprint_bytes, None);
    for expected_gauge in [
        ServerDiagnosticsGauge {
            name: "app.requests.in_flight".to_string(),
            value: 1,
        },
        ServerDiagnosticsGauge {
            name: "core.threads.live".to_string(),
            value: 1,
        },
    ] {
        assert_eq!(
            diagnostics
                .gauges
                .iter()
                .find(|gauge| gauge.name == expected_gauge.name),
            Some(&expected_gauge)
        );
    }

    Ok(())
}

#[tokio::test]
async fn server_diagnostics_requires_experimental_capability() -> Result<()> {
    let codex_home = TempDir::new()?;
    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build()
        .await?;
    let initialization = app_server
        .initialize_with_capabilities(
            ClientInfo {
                name: DEFAULT_CLIENT_NAME.to_string(),
                title: None,
                version: "0.1.0".to_string(),
            },
            Some(InitializeCapabilities {
                experimental_api: false,
                ..Default::default()
            }),
        )
        .await?;
    assert!(matches!(initialization, JSONRPCMessage::Response(_)));

    let request_id = app_server
        .send_raw_request("server/diagnostics", Some(json!({})))
        .await?;
    let error = timeout(
        READ_TIMEOUT,
        app_server.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(error.error.code, -32600);
    assert_eq!(
        error.error.message,
        "server/diagnostics requires experimentalApi capability"
    );
    assert_eq!(error.error.data, None);

    Ok(())
}
