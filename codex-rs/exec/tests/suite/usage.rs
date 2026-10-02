use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex_exec::test_codex_exec;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use walkdir::WalkDir;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn harbor_shaped_exec_includes_real_luna_reduction_in_root_usage() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    let test = test_codex_exec();
    let server = responses::start_mock_server().await;
    let completed = |id: &str| {
        json!({
            "type": "response.completed",
            "response": { "id": id, "usage": {
                "input_tokens": 10,
                "input_tokens_details": { "cached_tokens": 3, "cache_write_tokens": 4 },
                "output_tokens": 29,
                "output_tokens_details": { "reasoning_tokens": 7 },
                "total_tokens": 42
            }}
        })
    };
    let exchange = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("root-tool"),
                responses::ev_custom_tool_call(
                    "eval-exec",
                    "exec",
                    "notify('sensitive-' + 'notification'); text('sensitive-' + 'output');",
                ),
                completed("root-tool"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("luna-reduction"),
                responses::ev_assistant_message(
                    "luna-result",
                    r#"{"decision":"replace","replacement":"selected eval fact"}"#,
                ),
                completed("luna-reduction"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("root-final"),
                responses::ev_assistant_message("root-answer", "done"),
                completed("root-final"),
            ]),
        ],
    )
    .await;
    let mut command = assert_cmd::Command::new(codex_utils_cargo_bin::cargo_bin("codex")?);
    command
        .current_dir(test.cwd_path())
        .env("CODEX_HOME", test.home_path())
        .env("CODEX_SQLITE_HOME", test.home_path())
        .env("CODEX_API_KEY", "dummy")
        .arg("exec")
        .args([
            "-c",
            &format!("openai_base_url={:?}", format!("{}/v1", server.uri())),
        ]);
    let output = command
        .args([
            "--dangerously-bypass-approvals-and-sandbox",
            "--skip-git-repo-check",
            "--json",
            "--enable",
            "unified_exec",
        ])
        .args([
            "-c",
            "features.code_mode.enabled=true",
            "-c",
            "features.code_mode_host.enabled=true",
        ])
        .args(["-c", "features.code_mode.token_miser.enabled=true"])
        .args(["--", "inspect the tool result"])
        .output()?;
    assert!(output.status.success(), "exec failed: {output:?}");
    let requests = exchange.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].body_json()["model"], "gpt-6-luna");
    assert!(
        requests[1]
            .body_json()
            .to_string()
            .contains("<token_miser_input>")
    );
    let root_output = requests[2]
        .inputs_of_type("custom_tool_call_output")
        .iter()
        .map(|item| item["output"].to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(root_output.contains("selected eval fact"));
    assert!(!root_output.contains("sensitive-output"));
    assert!(!root_output.contains("sensitive-notification"));
    let stdout = String::from_utf8(output.stdout)?;
    assert!(!stdout.contains("sensitive-output"));
    assert!(!stdout.contains("sensitive-notification"));
    let events = stdout
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    let expected = json!({
        "input_tokens": 30, "cached_input_tokens": 9, "cache_write_input_tokens": 12,
        "output_tokens": 87, "reasoning_output_tokens": 21, "total_tokens": 126
    });
    let completed_events = events
        .iter()
        .filter(|item| item["type"] == "turn.completed")
        .collect::<Vec<_>>();
    assert_eq!(
        completed_events,
        vec![&json!({"type": "turn.completed", "usage": expected})]
    );
    let root_items = WalkDir::new(test.home_path().join("sessions"))
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.file_type().is_file()
                && entry.path().extension().is_some_and(|ext| ext == "jsonl")
        })
        .map(|entry| -> anyhow::Result<Vec<Value>> {
            Ok(std::fs::read_to_string(entry.path())?
                .lines()
                .map(serde_json::from_str)
                .collect::<Result<_, _>>()?)
        })
        .collect::<anyhow::Result<Vec<_>>>()?
        .into_iter()
        .find(|items| {
            items
                .iter()
                .any(|item| item["type"] == "token_miser_output")
        })
        .expect("root rollout retains exact hidden output");
    assert_eq!(
        root_items
            .iter()
            .filter(|item| item["type"] == "token_miser_decision")
            .count(),
        1
    );
    let mut raw_content = root_items
        .iter()
        .filter(|item| item["type"] == "token_miser_output")
        .map(|item| item["payload"]["content_items"].clone())
        .collect::<Vec<_>>();
    raw_content.sort_by_key(Value::to_string);
    assert_eq!(
        raw_content,
        vec![
            json!([{"type": "input_text", "text": "sensitive-notification"}]),
            json!([{"type": "input_text", "text": "sensitive-output"}]),
        ]
    );
    let total = root_items.iter().rev().find_map(|item| {
        (item["type"] == "event_msg" && item["payload"]["type"] == "token_count")
            .then(|| item["payload"]["info"]["total_token_usage"].clone())
    });
    assert_eq!(total, Some(expected));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn harbor_shaped_exec_json_and_rollout_preserve_authoritative_usage() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let test = test_codex_exec();
    let server = responses::start_mock_server().await;
    let response = responses::sse(vec![
        responses::ev_response_created("usage-response"),
        responses::ev_assistant_message("usage-message", "done"),
        json!({
            "type": "response.completed",
            "response": {
                "id": "usage-response",
                "usage": {
                    "input_tokens": 10,
                    "input_tokens_details": {
                        "cached_tokens": 3,
                        "cache_write_tokens": 4
                    },
                    "output_tokens": 29,
                    "output_tokens_details": { "reasoning_tokens": 7 },
                    "total_tokens": 42
                }
            }
        }),
    ]);
    let _response_mock = responses::mount_sse_once(&server, response).await;

    let output = test
        .cmd_with_server(&server)
        .arg("--skip-git-repo-check")
        .arg("--json")
        .arg("report usage")
        .output()?;
    assert!(output.status.success(), "exec run failed: {output:?}");

    let events = String::from_utf8(output.stdout)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    let completed = events
        .iter()
        .filter(|event| event["type"] == "turn.completed")
        .collect::<Vec<_>>();
    assert_eq!(
        completed,
        vec![&json!({
            "type": "turn.completed",
            "usage": {
                "total_tokens": 42,
                "input_tokens": 10,
                "cached_input_tokens": 3,
                "cache_write_input_tokens": 4,
                "output_tokens": 29,
                "reasoning_output_tokens": 7
            }
        })]
    );

    let rollout_path = WalkDir::new(test.home_path().join("sessions"))
        .into_iter()
        .filter_map(Result::ok)
        .find(|entry| {
            entry.file_type().is_file() && entry.file_name().to_string_lossy().ends_with(".jsonl")
        })
        .expect("codex exec should persist one rollout")
        .into_path();
    let rollout_items = std::fs::read_to_string(rollout_path)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    let total = rollout_items.iter().rev().find_map(|item| {
        (item["type"] == "event_msg" && item["payload"]["type"] == "token_count")
            .then(|| item["payload"]["info"]["total_token_usage"].clone())
            .filter(|usage| !usage.is_null())
    });
    assert_eq!(
        total,
        Some(json!({
            "input_tokens": 10,
            "cached_input_tokens": 3,
            "cache_write_input_tokens": 4,
            "output_tokens": 29,
            "reasoning_output_tokens": 7,
            "total_tokens": 42
        }))
    );

    Ok(())
}
