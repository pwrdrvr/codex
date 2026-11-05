use base64::Engine;
use codex_app_server_protocol::ClientResponsePayload;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::ThreadExportParams;
use codex_app_server_protocol::ThreadExportResponse;
use codex_app_server_protocol::ThreadStatus;
use codex_protocol::ThreadId;
use codex_thread_store::LocalThreadStore;

use super::ThreadRequestProcessor;
use super::thread_processor::unsupported_thread_store_operation;
use crate::error_code::internal_error;
use crate::error_code::invalid_request;

impl ThreadRequestProcessor {
    pub(crate) async fn thread_export(
        &self,
        params: ThreadExportParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let thread_id = ThreadId::from_string(&params.thread_id)
            .map_err(|error| invalid_request(format!("invalid thread id: {error}")))?;
        let max_bytes = params.max_bytes.unwrap_or(64 * 1024 * 1024);
        if max_bytes == 0 || max_bytes > 128 * 1024 * 1024 {
            return Err(invalid_request(
                "thread/export maxBytes must be between 1 and 134217728",
            ));
        }
        if matches!(
            self.thread_watch_manager
                .loaded_status_for_thread(&params.thread_id)
                .await,
            ThreadStatus::Active { .. }
        ) {
            return Err(invalid_request(
                "Wait for the source turn to finish before exporting history",
            ));
        }
        let store = self
            .thread_store
            .as_any()
            .downcast_ref::<LocalThreadStore>()
            .ok_or_else(|| unsupported_thread_store_operation("thread/export"))?;
        let bytes = store
            .export_thread_rollout(thread_id, max_bytes as usize)
            .await
            .map_err(|error| internal_error(error.to_string()))?;
        Ok(Some(
            ThreadExportResponse {
                thread_id: params.thread_id,
                format: "legacy-rollout-v1".to_string(),
                data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
            }
            .into(),
        ))
    }
}
