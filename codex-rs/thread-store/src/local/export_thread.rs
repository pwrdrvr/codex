//! Produces self-contained rollout history without exposing storage layout to clients.

use codex_protocol::ThreadId;
use codex_protocol::protocol::ThreadHistoryMode;
use codex_rollout::ReverseJsonlScanner;
use codex_rollout::RolloutItem;
use codex_rollout::RolloutLine;
use codex_rollout::ScanOutcome;

use super::LocalThreadStore;
use super::read_thread;
use crate::ReadThreadParams;
use crate::ThreadStore;
use crate::ThreadStoreError;
use crate::ThreadStoreResult;

impl LocalThreadStore {
    /// Flatten the complete persisted lineage, including compaction and tool records.
    /// Only the exported metadata changes; the source is never rewritten.
    pub async fn export_thread_rollout(
        &self,
        thread_id: ThreadId,
        max_bytes: usize,
    ) -> ThreadStoreResult<Vec<u8>> {
        if self.live_recorders.lock().await.contains_key(&thread_id) {
            self.flush_thread(thread_id).await?;
        }
        let _writer_guard = self.live_writer_locks.lock(thread_id).await;
        let stored = read_thread::read_thread(
            self,
            ReadThreadParams {
                thread_id,
                include_archived: false,
                include_history: false,
            },
        )
        .await?;
        let path = stored
            .rollout_path
            .ok_or_else(|| ThreadStoreError::InvalidRequest {
                message: "thread/export requires persisted history".to_string(),
            })?;
        let mut meta = codex_rollout::read_session_meta_line(&path)
            .await
            .map_err(export_error)?;
        if meta.meta.id != thread_id {
            return Err(ThreadStoreError::InvalidRequest {
                message: "thread/export source identity changed".to_string(),
            });
        }
        if meta.meta.subagent_history_start_ordinal.is_some() {
            return Err(ThreadStoreError::Unsupported {
                operation: "thread/export with inherited subagent-only projection",
            });
        }
        let segments = if meta.meta.history_mode == ThreadHistoryMode::Paginated {
            self.resolve_rollout_lineage(thread_id, Some(path))
                .await?
                .segments
                .into_iter()
                .map(|segment| {
                    (
                        segment.rollout_path,
                        segment.end.map(|end| end.end_byte_offset),
                    )
                })
                .collect::<Vec<_>>()
        } else {
            vec![(path, None)]
        };
        meta.meta.history_mode = ThreadHistoryMode::Legacy;
        meta.meta.history_base = None;
        meta.meta.forked_from_ordinal_exclusive = None;
        let metadata = RolloutLine {
            timestamp: meta.meta.timestamp.clone(),
            ordinal: None,
            item: RolloutItem::SessionMeta(meta),
        };
        tokio::task::spawn_blocking(move || {
            let mut output = Vec::new();
            append_line(&mut output, &metadata, max_bytes)?;
            for (path, end) in segments {
                let file =
                    codex_rollout::open_rollout_seekable_reader(&path).map_err(export_error)?;
                let mut scanner = match end {
                    Some(end) => ReverseJsonlScanner::new_at(file, end),
                    None => ReverseJsonlScanner::new(file),
                }
                .map_err(export_error)?;
                let mut lines = Vec::new();
                let mut segment_bytes = 0usize;
                while let Some(outcome) = scanner.scan_next_rollout_line().map_err(export_error)? {
                    let mut line = match outcome {
                        ScanOutcome::Parsed(line) => line,
                        ScanOutcome::Rejected(error) => return Err(export_error(error)),
                    };
                    if matches!(line.item, RolloutItem::SessionMeta(_)) {
                        break;
                    }
                    line.ordinal = None;
                    let mut bytes = Vec::new();
                    append_line(&mut bytes, &line, max_bytes)?;
                    segment_bytes = segment_bytes.saturating_add(bytes.len());
                    if output.len().saturating_add(segment_bytes) > max_bytes {
                        return Err(export_too_large());
                    }
                    lines.push(bytes);
                }
                for bytes in lines.into_iter().rev() {
                    output.extend_from_slice(&bytes);
                }
            }
            Ok(output)
        })
        .await
        .map_err(export_error)?
    }
}

fn append_line(
    output: &mut Vec<u8>,
    line: &RolloutLine,
    max_bytes: usize,
) -> ThreadStoreResult<()> {
    serde_json::to_writer(&mut *output, line).map_err(export_error)?;
    output.push(b'\n');
    if output.len() > max_bytes {
        return Err(export_too_large());
    }
    Ok(())
}

fn export_too_large() -> ThreadStoreError {
    ThreadStoreError::InvalidRequest {
        message: "thread/export exceeds maxBytes".to_string(),
    }
}

fn export_error(error: impl std::fmt::Display) -> ThreadStoreError {
    ThreadStoreError::Internal {
        message: format!("could not export complete thread history: {error}"),
    }
}

#[cfg(test)]
#[path = "export_thread_tests.rs"]
mod tests;
