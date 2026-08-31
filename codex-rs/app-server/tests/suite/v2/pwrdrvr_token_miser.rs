use anyhow::Context;
use anyhow::Result;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use codex_app_server_protocol::ClientInfo;
use codex_app_server_protocol::InitializeCapabilities;
use codex_app_server_protocol::JSONRPCMessage;
use codex_app_server_protocol::PwrdrvrTokenMiserActivation;
use codex_app_server_protocol::PwrdrvrTokenMiserInitializeCapability;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::UserInput;
use codex_features::Feature;
use core_test_support::responses;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

const ACTIVATION_NONCE: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const BRIDGE_TOKEN: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const ORIGINAL_OUTPUT: &str = "managed-original-output";
const REPLACEMENT: &str = "managed Token Miser replacement";

fn output_text(output: &Value) -> String {
    match output {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| item["text"].as_str())
            .collect::<Vec<_>>()
            .join(""),
        Value::Object(_) => output_text(&output["content"]),
        other => other.to_string(),
    }
}

fn write_descriptor(directory: &TempDir, bridge: &MockServer) -> Result<std::path::PathBuf> {
    let descriptor_path = directory.path().join("token-miser-bridge-v1.json");
    std::fs::write(
        &descriptor_path,
        serde_json::to_vec(&json!({
            "version": 1,
            "identity": "pwrdrvr.pwragent.token-miser",
            "activation_nonce": ACTIVATION_NONCE,
            "url": format!("{}/v1/post-tool-use", bridge.uri()),
            "acceptance_url": format!("{}/v1/accept-code-mode-output", bridge.uri()),
            "token": BRIDGE_TOKEN,
        }))?,
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))?;
        std::fs::set_permissions(&descriptor_path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(descriptor_path)
}

async fn initialize_pwragent(app_server: &mut TestAppServer) -> Result<()> {
    let initialized = app_server
        .initialize_with_capabilities(
            ClientInfo {
                name: "pwragent-desktop".to_string(),
                title: Some("PwrAgent".to_string()),
                version: "test".to_string(),
            },
            Some(InitializeCapabilities {
                experimental_api: true,
                pwrdrvr_token_miser: Some(PwrdrvrTokenMiserInitializeCapability {
                    version: 1,
                    activation_nonce: ACTIVATION_NONCE.to_string(),
                }),
                ..Default::default()
            }),
        )
        .await?;
    assert!(matches!(initialized, JSONRPCMessage::Response(_)));
    Ok(())
}

async fn run_direct_exec(bridge_response: ResponseTemplate) -> Result<(Value, Vec<Value>, Value)> {
    let bridge = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/post-tool-use"))
        .and(header("authorization", format!("Bearer {BRIDGE_TOKEN}")))
        .respond_with(bridge_response)
        .expect(1)
        .mount(&bridge)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/accept-code-mode-output"))
        .and(header("authorization", format!("Bearer {BRIDGE_TOKEN}")))
        .respond_with(ResponseTemplate::new(204))
        .mount(&bridge)
        .await;

    let model_server = responses::start_mock_server().await;
    let response_mock = responses::mount_sse_sequence(
        &model_server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("resp-1"),
                responses::ev_function_call(
                    "managed-call",
                    "exec_command",
                    &json!({
                        "cmd": format!("echo {ORIGINAL_OUTPUT}"),
                        "login": false,
                        "yield_time_ms": 30_000,
                    })
                    .to_string(),
                ),
                responses::ev_completed("resp-1"),
            ]),
            responses::sse(vec![
                responses::ev_assistant_message("msg-1", "Done"),
                responses::ev_completed("resp-2"),
            ]),
        ],
    )
    .await;
    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(&model_server.uri()).write(codex_home.path())?;
    let ordinary_hook_input = write_observing_post_tool_use_hook(&codex_home)?;
    let descriptor_directory = TempDir::new()?;
    let descriptor_path = write_descriptor(&descriptor_directory, &bridge)?;
    let descriptor_path = descriptor_path
        .to_str()
        .context("descriptor path must be UTF-8")?;
    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&[(
            "PWRAGENT_TOKEN_MISER_BRIDGE_DESCRIPTOR_PATH",
            Some(descriptor_path),
        )])
        .build()
        .await?;
    initialize_pwragent(&mut app_server).await?;
    let started = app_server
        .start_thread(ThreadStartParams {
            pwrdrvr_token_miser: Some(PwrdrvrTokenMiserActivation {
                version: 1,
                enabled: true,
            }),
            ..Default::default()
        })
        .await?;
    app_server
        .start_turn_and_wait_for_completion(TurnStartParams {
            thread_id: started.thread.id,
            input: vec![UserInput::Text {
                text: "run the command".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;

    let model_requests = response_mock.requests();
    assert_eq!(model_requests.len(), 2);
    let model_output = model_requests[1].function_call_output("managed-call");
    let bridge_requests = bridge
        .received_requests()
        .await
        .context("bridge request recording is enabled")?
        .into_iter()
        .map(|request| serde_json::from_slice(&request.body))
        .collect::<Result<Vec<Value>, _>>()?;
    let ordinary_hook_input = serde_json::from_slice(&std::fs::read(ordinary_hook_input)?)?;
    Ok((model_output, bridge_requests, ordinary_hook_input))
}

fn write_observing_post_tool_use_hook(codex_home: &TempDir) -> Result<std::path::PathBuf> {
    let script_path = codex_home.path().join("observe-post-tool-use.py");
    let input_path = codex_home.path().join("ordinary-post-tool-use-input.json");
    std::fs::write(
        &script_path,
        format!(
            r#"import sys
from pathlib import Path

Path(r"{input_path}").write_bytes(sys.stdin.buffer.read())
"#,
            input_path = input_path.display(),
        ),
    )?;
    std::fs::write(
        codex_home.path().join("requirements.toml"),
        format!(
            r#"[hooks]

[[hooks.PostToolUse]]
matcher = '^Bash$'

[[hooks.PostToolUse.hooks]]
type = 'command'
command = 'python3 {script_path}'
"#,
            script_path = script_path.display(),
        ),
    )?;
    Ok(input_path)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_activation_replaces_exact_direct_output_without_installing_a_hook() -> Result<()> {
    let (model_output, bridge_requests, ordinary_hook_input) =
        run_direct_exec(ResponseTemplate::new(200).set_body_json(json!({
            "hookOutput": {
                "continue": false,
                "stopReason": REPLACEMENT,
                "hookSpecificOutput": {
                    "hookEventName": "PostToolUse",
                    "response_id": "managed-response-1",
                }
            }
        })))
        .await?;

    let model_output = model_output.to_string();
    assert!(model_output.contains(REPLACEMENT));
    assert!(!model_output.contains(ORIGINAL_OUTPUT));
    assert_eq!(bridge_requests.len(), 2);
    let post_tool_use = &bridge_requests[0];
    assert_eq!(post_tool_use["hook_event_name"], "PostToolUse");
    assert_eq!(post_tool_use["tool_name"], "Bash");
    assert_eq!(post_tool_use["token_miser_exact_tool_response_version"], 1);
    assert!(
        post_tool_use["token_miser_exact_tool_response"]
            .to_string()
            .contains(ORIGINAL_OUTPUT)
    );
    assert_eq!(
        ordinary_hook_input.get("token_miser_exact_tool_response"),
        None,
        "managed activation must not expand ordinary-hook exact-output access"
    );
    assert_eq!(
        bridge_requests[1],
        json!({
            "version": 1,
            "response_id": "managed-response-1",
            "session_id": post_tool_use["session_id"],
            "turn_id": post_tool_use["turn_id"],
            "tool_use_id": "managed-call",
        })
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_bridge_failure_fails_open_to_original_tool_output() -> Result<()> {
    let (model_output, bridge_requests, _) = run_direct_exec(ResponseTemplate::new(503)).await?;

    let model_output = model_output.to_string();
    assert!(model_output.contains(ORIGINAL_OUTPUT));
    assert!(!model_output.contains(REPLACEMENT));
    assert_eq!(bridge_requests.len(), 1);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_multibyte_replacement_over_byte_cap_fails_open() -> Result<()> {
    let oversized_replacement = "é".repeat(4_000);
    let (model_output, bridge_requests, _) =
        run_direct_exec(ResponseTemplate::new(200).set_body_json(json!({
            "hookOutput": {
                "continue": false,
                "stopReason": oversized_replacement,
                "hookSpecificOutput": {
                    "hookEventName": "PostToolUse",
                    "response_id": "managed-response-oversized",
                }
            }
        })))
        .await?;

    let model_output = model_output.to_string();
    assert!(model_output.contains(ORIGINAL_OUTPUT));
    assert!(!model_output.contains(&"é".repeat(4_000)));
    assert_eq!(bridge_requests.len(), 1);
    Ok(())
}

#[cfg_attr(windows, ignore = "no exec_command on Windows")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_gate_does_not_delay_or_intercept_nested_code_mode_tools() -> Result<()> {
    let bridge = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/post-tool-use"))
        .respond_with(ResponseTemplate::new(200).set_delay(std::time::Duration::from_secs(2)))
        .mount(&bridge)
        .await;

    let model_server = responses::start_mock_server().await;
    let response_mock = responses::mount_sse_sequence(
        &model_server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("resp-code-mode-1"),
                responses::ev_custom_tool_call(
                    "managed-code-mode-call",
                    "exec",
                    r#"// @exec: {"yield_time_ms": 1000}
const result = await tools.exec_command({ cmd: "printf managed-fast-read", yield_time_ms: 30000 });
text(result.output);
"#,
                ),
                responses::ev_completed("resp-code-mode-1"),
            ]),
            responses::sse(vec![
                responses::ev_assistant_message("msg-code-mode-done", "Done"),
                responses::ev_completed("resp-code-mode-2"),
            ]),
        ],
    )
    .await;
    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(&model_server.uri())
        .with_model("test-gpt-5.1-codex")
        .enable_feature(Feature::CodeMode)
        .write(codex_home.path())?;
    let descriptor_directory = TempDir::new()?;
    let descriptor_path = write_descriptor(&descriptor_directory, &bridge)?;
    let descriptor_path = descriptor_path
        .to_str()
        .context("descriptor path must be UTF-8")?;
    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&[(
            "PWRAGENT_TOKEN_MISER_BRIDGE_DESCRIPTOR_PATH",
            Some(descriptor_path),
        )])
        .build()
        .await?;
    initialize_pwragent(&mut app_server).await?;
    let started = app_server
        .start_thread(ThreadStartParams {
            pwrdrvr_token_miser: Some(PwrdrvrTokenMiserActivation {
                version: 1,
                enabled: true,
            }),
            ..Default::default()
        })
        .await?;
    app_server
        .start_turn_and_wait_for_completion(TurnStartParams {
            thread_id: started.thread.id,
            input: vec![UserInput::Text {
                text: "run the fast Code Mode read".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;

    let model_requests = response_mock.requests();
    assert_eq!(model_requests.len(), 2);
    let output =
        output_text(&model_requests[1].custom_tool_call_output("managed-code-mode-call")["output"]);
    assert!(
        output.contains("Script completed") && output.contains("managed-fast-read"),
        "a fast nested read must complete in the initial outer cell: {output}"
    );
    assert!(
        !output.contains("Script running with cell ID"),
        "the managed gate must not push a fast nested read past the outer yield window: {output}"
    );
    let bridge_requests = bridge
        .received_requests()
        .await
        .context("bridge request recording is enabled")?;
    assert!(
        bridge_requests.is_empty(),
        "nested Code Mode results are private script inputs, not model-visible gate boundaries"
    );

    Ok(())
}
