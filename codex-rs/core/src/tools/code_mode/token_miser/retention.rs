//! Delivers authorized retrieval fragments independently of untrusted JavaScript output.

use super::*;
use crate::context::TokenMiserRetrievalResult;

const MAX_CELL_RETRIEVAL_BYTES: usize = 8 * 1024;

impl TokenMiserService {
    pub(in crate::tools::code_mode) fn record_retrieval(
        &self,
        cell_id: &str,
        result: &Value,
    ) -> Result<(), String> {
        let body = result.to_string();
        if body.len() > MAX_CELL_RETRIEVAL_BYTES - 128 {
            return Err("retrieval result exceeds its framed byte bound".to_string());
        }
        let framed = TokenMiserRetrievalResult::new(body).render();
        let mut cells = self
            .explicit_retrieval_cells
            .lock()
            .map_err(|_| "retrieval delivery is unavailable".to_string())?;
        let fragments = cells.entry(cell_id.to_string()).or_default();
        if fragments.iter().map(String::len).sum::<usize>() + framed.len()
            > MAX_CELL_RETRIEVAL_BYTES
        {
            return Err(
                "this cell's retrieval delivery is full; use a separate read call if needed"
                    .to_string(),
            );
        }
        fragments.push(framed);
        Ok(())
    }

    pub(in crate::tools::code_mode) fn take_retrieval(
        &self,
        cell_id: &str,
    ) -> Option<Vec<FunctionCallOutputContentItem>> {
        self.explicit_retrieval_cells
            .lock()
            .ok()?
            .remove(cell_id)
            .map(|fragments| {
                fragments
                    .into_iter()
                    .map(|text| FunctionCallOutputContentItem::InputText { text })
                    .collect()
            })
    }

    /// Retain a live observation or mixed retrieval cell without submitting it to Luna.
    /// Only a durable receipt and separately authorized retrieval data can become visible.
    pub(in crate::tools::code_mode) async fn retain_unreduced(
        &self,
        session: &Arc<Session>,
        context: &ReductionContext,
        output: TerminalOutput<'_>,
    ) -> Vec<FunctionCallOutputContentItem> {
        if output.content_items.is_empty() {
            return Vec::new();
        }
        if !output_fits_storage_bound(
            [
                context.turn_id.as_str(),
                context.call_id.as_str(),
                context.cell_id.as_str(),
                output.script_status,
            ],
            &output.content_items,
            MAX_PERSISTED_OUTPUT_ITEMS,
            MAX_PERSISTED_OUTPUT_SOURCE_BYTES,
        ) {
            return receipt_output(TokenMiserReceipt::output_too_large());
        }
        let raw = Arc::new(TokenMiserOutput {
            version: TOKEN_MISER_VERSION,
            object_id: Uuid::new_v4().to_string(),
            thread_id: session.thread_id,
            turn_id: context.turn_id.clone(),
            call_id: context.call_id.clone(),
            cell_id: context.cell_id.clone(),
            script_status: output.script_status.to_string(),
            success: output.success,
            content_items: output.content_items,
        });
        let object_id = raw.object_id.clone();
        let session = Arc::clone(session);
        let outputs = Arc::clone(&self.outputs);
        let persisted = self
            .pending_tasks
            .spawn(async move {
                if !session.persist_token_miser_output(Arc::clone(&raw)).await {
                    return false;
                }
                let Ok(mut outputs) = outputs.write() else {
                    return false;
                };
                outputs.insert(raw.object_id.clone(), raw);
                true
            })
            .await
            .unwrap_or(false);
        receipt_output(if persisted {
            TokenMiserReceipt::retained(&object_id, "retained")
        } else {
            TokenMiserReceipt::storage_unavailable()
        })
    }
}
