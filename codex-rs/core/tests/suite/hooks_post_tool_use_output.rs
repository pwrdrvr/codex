use std::fs;
use std::path::Path;

use anyhow::Context;
use anyhow::Result;
use codex_features::Feature;
use core_test_support::hooks::trust_discovered_hooks;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;

const OUTPUT_START: &str = "post-tool-use-raw-start";
const OUTPUT_END: &str = "post-tool-use-raw-end";
const REPLACEMENT: &str = "PostToolUse replaced the oversized shell result";

#[derive(Clone, Copy)]
enum ShellTool {
    ShellCommand,
    UnifiedExec,
}

#[tokio::test]
async fn shell_command_post_tool_use_receives_legacy_truncated_output() -> Result<()> {
    assert_post_tool_use_receives_legacy_truncated_output(ShellTool::ShellCommand).await
}

#[tokio::test]
async fn unified_exec_post_tool_use_receives_legacy_truncated_output() -> Result<()> {
    assert_post_tool_use_receives_legacy_truncated_output(ShellTool::UnifiedExec).await
}

async fn assert_post_tool_use_receives_legacy_truncated_output(
    shell_tool: ShellTool,
) -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let call_id = match shell_tool {
        ShellTool::ShellCommand => "post-tool-use-raw-shell-command",
        ShellTool::UnifiedExec => "post-tool-use-raw-unified-exec",
    };
    let command = format!(r#"python3 -c 'print("{OUTPUT_START}" + "x" * 60000 + "{OUTPUT_END}")'"#);
    let (tool_name, args) = match shell_tool {
        ShellTool::ShellCommand => ("shell_command", json!({ "command": command })),
        ShellTool::UnifiedExec => (
            "exec_command",
            json!({
                "cmd": command,
                "max_output_tokens": 5,
                "yield_time_ms": 10_000,
            }),
        ),
    };
    let responses = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_function_call(call_id, tool_name, &serde_json::to_string(&args)?),
                ev_completed("resp-1"),
            ]),
            sse(vec![
                ev_response_created("resp-2"),
                ev_assistant_message("msg-1", "replacement observed"),
                ev_completed("resp-2"),
            ]),
        ],
    )
    .await;

    let mut builder = test_codex()
        .with_pre_build_hook(write_replacing_post_tool_use_hook)
        .with_config(move |config| {
            if matches!(shell_tool, ShellTool::UnifiedExec) {
                config.use_experimental_unified_exec_tool = true;
                config
                    .features
                    .enable(Feature::UnifiedExec)
                    .expect("test config should allow unified exec");
            }
            trust_discovered_hooks(config);
        });
    let test = builder.build(&server).await?;

    test.submit_turn("produce oversized shell output for the post-tool hook")
        .await?;

    let requests = responses.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].function_call_output(call_id)["output"],
        Value::String(REPLACEMENT.to_string())
    );

    let hook_payload: Value = serde_json::from_str(&fs::read_to_string(
        test.codex_home_path().join("post_tool_use_hook_input.json"),
    )?)?;
    let tool_response = hook_payload["tool_response"]
        .as_str()
        .context("PostToolUse tool_response should be a string")?;
    assert!(tool_response.contains(OUTPUT_START));
    assert!(tool_response.contains(OUTPUT_END));
    assert!(tool_response.contains("tokens truncated"));
    assert!(tool_response.len() < 60_000);
    assert_eq!(
        hook_payload.get("token_miser_exact_tool_response"),
        None,
        "exact output is negotiated only when a reducer is configured"
    );

    Ok(())
}

fn write_replacing_post_tool_use_hook(home: &Path) {
    let script_path = home.join("post_tool_use_hook.py");
    let input_path = home.join("post_tool_use_hook_input.json");
    let replacement = serde_json::to_string(REPLACEMENT).expect("serialize replacement");
    let script = format!(
        r#"import json
from pathlib import Path
import sys

payload = json.load(sys.stdin)
Path(r"{input_path}").write_text(json.dumps(payload), encoding="utf-8")
print(json.dumps({{"continue": False, "stopReason": {replacement}}}))
"#,
        input_path = input_path.display(),
    );
    let hooks = json!({
        "hooks": {
            "PostToolUse": [{
                "matcher": "^Bash$",
                "hooks": [{
                    "type": "command",
                    "command": format!("python3 {}", script_path.display()),
                    "statusMessage": "replacing oversized shell output",
                }]
            }]
        }
    });

    fs::write(&script_path, script).expect("write PostToolUse test hook");
    fs::write(home.join("hooks.json"), hooks.to_string()).expect("write hooks config");
}
