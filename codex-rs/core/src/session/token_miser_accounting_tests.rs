use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn token_miser_storage_failure_does_not_erase_incurred_usage() {
    let (mut session, turn) = make_session_and_context().await;
    let rollout_path = attach_thread_persistence(&mut session).await;
    session
        .live_thread()
        .expect("writer")
        .shutdown()
        .await
        .expect("close writer");
    let usage = TokenUsage {
        input_tokens: 11,
        cached_input_tokens: 3,
        cache_write_input_tokens: 2,
        output_tokens: 7,
        reasoning_output_tokens: 4,
        total_tokens: 18,
        codex_rollout_budget_units: None,
    };
    let committed = session
        .commit_token_miser_decision(
            &turn,
            codex_history::TokenMiserDecisionRecord {
                version: 1,
                object_id: Uuid::new_v4().to_string(),
                thread_id: session.thread_id,
                turn_id: turn.sub_id.clone(),
                call_id: "closed-writer-call".to_string(),
                cell_id: "closed-writer-cell".to_string(),
                outcome: codex_history::TokenMiserStoredOutcome::Hide {
                    reason: "test".to_string(),
                },
                usage: Some(usage.clone()),
            },
        )
        .await;
    assert!(
        !committed,
        "a closed writer cannot commit a durable decision"
    );
    assert_eq!(session.total_token_usage().await, Some(usage.clone()));
    assert!(!session.flush_pending_token_miser_decisions().await);

    // Storage recovers before shutdown. Later root usage must not be overwritten by the older
    // failed snapshot, and retrying persistence must not charge the reducer a second time.
    session
        .state
        .lock()
        .await
        .history
        .add_background_token_usage(&usage);
    let mut expected = usage.clone();
    expected.add_assign(&usage);
    let config = session.get_config().await;
    session.services.live_thread = Some(
        LiveThread::resume(
            Arc::clone(&session.services.thread_store),
            codex_protocol::protocol::ThreadHistoryMode::Legacy,
            codex_thread_store::ResumeThreadParams {
                thread_id: session.thread_id,
                rollout_path: Some(rollout_path.clone()),
                history: None,
                include_archived: true,
                metadata: ThreadPersistenceMetadata {
                    cwd: Some(config.cwd.to_path_buf()),
                    model_provider: config.model_provider_id.clone(),
                    memory_mode: ThreadMemoryMode::Enabled,
                },
            },
        )
        .await
        .expect("reopen recovered writer"),
    );
    assert!(session.flush_pending_token_miser_decisions().await);
    assert!(session.flush_pending_token_miser_decisions().await);
    session
        .live_thread()
        .expect("writer")
        .shutdown()
        .await
        .expect("close recovered writer");
    let (items, _, errors) = RolloutRecorder::load_rollout_items(&rollout_path)
        .await
        .expect("durable history");
    assert_eq!(errors, 0);
    assert_eq!(
        items
            .iter()
            .filter(|item| matches!(item, RolloutItem::TokenMiserDecision(_)))
            .count(),
        1
    );
    let persisted_usage = items.iter().rev().find_map(|item| match item {
        RolloutItem::EventMsg(EventMsg::TokenCount(event)) => event
            .info
            .as_ref()
            .map(|info| info.total_token_usage.clone()),
        _ => None,
    });
    assert_eq!(persisted_usage, Some(expected.clone()));
    assert_eq!(session.total_token_usage().await, Some(expected));
}
