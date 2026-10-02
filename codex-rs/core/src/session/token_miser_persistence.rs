//! Retrying durable reducer accounting without charging provider usage again.

use codex_history::RolloutItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::TokenCountEvent;
use codex_thread_store::PersistContext;

use super::session::Session;

impl Session {
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "serialize decision commits and their current absolute usage snapshot; persistence does not re-enter session state"
    )]
    pub(crate) async fn flush_pending_token_miser_decisions(&self) -> bool {
        let internal_mode = self.get_config().await.code_mode.token_miser.is_some();
        let mut state = self.state.lock().await;
        if state.token_miser_pending_decisions.is_empty() && !internal_mode {
            return true;
        }
        let Some(live_thread) = self.live_thread() else {
            return false;
        };
        let mut decisions = state
            .token_miser_pending_decisions
            .values()
            .cloned()
            .collect::<Vec<_>>();
        decisions.sort_by(|left, right| left.object_id.cmp(&right.object_id));
        let mut items = decisions
            .into_iter()
            .map(RolloutItem::TokenMiserDecision)
            .collect::<Vec<_>>();
        let (info, rate_limits) = state.token_info_and_rate_limits();
        items.push(RolloutItem::EventMsg(EventMsg::TokenCount(
            TokenCountEvent { info, rate_limits },
        )));
        // A retry after an ambiguous persistence failure can repeat the same object-id record.
        // Catalog replay deduplicates that identity, and totals are absolute snapshots, not deltas.
        // Always snapshot current usage here, including final shutdown after all reducers drain:
        // an older failed batch or concurrent event delivery must never lower resumed totals.
        if let Err(error) = live_thread.append_items(&items).await {
            tracing::error!(%error, "failed to append Token Miser accounting; retained for retry");
            return false;
        }
        if let Err(error) = live_thread.persist(PersistContext::Standard).await {
            tracing::error!(%error, "failed to persist Token Miser accounting; retained for retry");
            return false;
        }
        state.token_miser_pending_decisions.clear();
        true
    }
}
