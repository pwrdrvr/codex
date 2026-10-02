use super::*;
use codex_history::InitialHistory;
use codex_history::ResumedHistory;
use codex_protocol::mcp::ClientMcpExtensions;
use codex_protocol::protocol::ThreadHistoryMode;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paginated_resume_retrieves_pre_yield_output_without_reducing_it() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let initial_exchange = responses::mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("live-parent"),
                ev_custom_tool_call("live-call", "exec", DEFERRED_SCRIPT),
                ev_completed("live-parent"),
            ]),
            sse(vec![
                ev_response_created("live-final"),
                ev_assistant_message("live-answer", "done"),
                ev_completed("live-final"),
            ]),
        ],
    )
    .await;
    let initial = token_miser_test_builder()
        .with_history_mode(ThreadHistoryMode::Paginated)
        .build_with_auto_env(&server)
        .await?;
    initial.submit_turn("inspect the live output").await?;
    assert_eq!(initial_exchange.requests().len(), 2);
    let thread_id = initial.session_configured.thread_id;
    let catalog = initial
        .thread_store
        .load_token_miser_items(LoadThreadHistoryParams {
            thread_id,
            include_archived: false,
        })
        .await?;
    let [RolloutItem::TokenMiserOutput(raw)] = catalog.as_slice() else {
        anyhow::bail!("expected one unreduced live object: {catalog:?}");
    };
    assert_eq!(
        raw.content_items,
        vec![ProtocolFunctionCallOutputContentItem::InputText {
            text: LIVE_PREVIEW.to_string(),
        }]
    );
    let retrieval_script = format!(
        "const result = await tools.read_token_miser_output({{ object_id: {:?}, item_index: 0, offset: 0, max_bytes: 4096 }}); text(JSON.stringify(result));",
        raw.object_id,
    );
    initial.codex.shutdown_and_wait().await?;
    initial.thread_manager.remove_thread(&thread_id).await;
    let context = initial
        .thread_store
        .load_latest_model_context(LoadThreadHistoryParams {
            thread_id,
            include_archived: false,
        })
        .await?;
    assert!(!serde_json::to_string(&context.items)?.contains(LIVE_PREVIEW));
    let resumed = initial
        .thread_manager
        .resume_thread_with_history(
            initial.config.clone(),
            InitialHistory::Resumed(ResumedHistory {
                conversation_id: thread_id,
                history: Arc::new(context.items),
                rollout_path: initial.codex.rollout_path(),
            }),
            initial.thread_manager.auth_manager(),
            /*parent_trace*/ None,
            ClientMcpExtensions::default(),
        )
        .await?;
    let retrieval_exchange = responses::mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("read-parent"),
                ev_custom_tool_call("read-live", "exec", &retrieval_script),
                ev_completed("read-parent"),
            ]),
            sse(vec![
                ev_response_created("read-final"),
                ev_assistant_message("read-answer", "done"),
                ev_completed("read-final"),
            ]),
        ],
    )
    .await;
    resumed
        .thread
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "read the retained live item".to_string(),
            text_elements: Vec::new(),
        }]))
        .await?;
    wait_for_event(&resumed.thread, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    let requests = retrieval_exchange.requests();
    assert_eq!(requests.len(), 2);
    let visible = tool_output_for_call(&requests[1].body_json(), "read-live");
    assert!(
        visible.contains(LIVE_PREVIEW),
        "retrieved output: {visible}"
    );
    assert!(!visible.contains(UNRETRIEVED_SECRET));
    resumed.thread.shutdown_and_wait().await?;
    Ok(())
}
