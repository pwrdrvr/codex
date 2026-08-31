//! Pluggable reduction at the code-mode script-to-model boundary.
//!
//! # Why here
//!
//! A nested tool result returned by [`super::call_nested_tool`] goes *into the running script*,
//! which may grep, parse, or count it. Substituting a summary there would silently break scripts
//! doing programmatic post-processing. What the model actually reads is what the script yields or
//! returns, which is exactly what [`super::handle_runtime_response`] funnels through
//! [`super::truncate_code_mode_result`] for all three [`codex_code_mode::RuntimeResponse`] arms.
//!
//! That funnel already destroys output — it is a token-budgeted truncation. Making it pluggable
//! preserves the script contract and changes only what enters context.
//!
//! # Guarantees
//!
//! - **Inert by default.** With no reducer and no ceiling configured, [`apply_output_reduction`]
//!   is the existing truncation call, byte for byte.
//! - **Fails open during reduction.** Missing descriptor, unreachable host, timeout, non-2xx,
//!   malformed body, oversized body, empty replacement — every one falls back to truncation. Once
//!   a valid replacement is committed, its acceptance callback response cannot revoke it.
//! - **Actionable state is Codex-owned.** A reducer must echo nested unified-exec
//!   continuation state exactly. Codex rejects a missing or conflicting echo and appends the
//!   authoritative bounded envelope outside both the replacement fence and replacement budget.
//! - **Budget always enforced.** The replacement is truncated with the same policy as the
//!   original, so a reducer cannot enlarge what reaches the model.
//! - **Replacement is untrusted.** See [`CodeModeOutputReplacementFence`].

use std::io;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use codex_http_client::HttpClient;
use codex_http_client::HttpClientBuilder;
use codex_protocol::models::FunctionCallOutputContentItem;
use futures::future::BoxFuture;
use serde::Deserialize;
use serde::Serialize;
use url::Host;
use url::Url;

use crate::config::CodeModeOutputReducerConfig;
use crate::context::CodeModeOutputReductionGuidance;
use crate::context::CodeModeOutputReplacementFence;
use crate::context::ContextualUserFragment;
use crate::tools::code_mode::actionable_state::ActionableState;
use crate::unified_exec::resolve_max_tokens;

/// Wire version for both the descriptor file and the reduction request/response envelopes.
const REDUCER_PROTOCOL_VERSION: u32 = 1;
const ACCEPTANCE_MAX_ATTEMPTS: u32 = 2;
const ACCEPTANCE_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(50);
const MAX_MODEL_CONTEXT_ITEM_TOKENS: usize = 10_000;

#[cfg(test)]
use crate::context::CODE_MODE_OUTPUT_REPLACEMENT_FOOTER as UNTRUSTED_REPLACEMENT_FOOTER;
#[cfg(test)]
use crate::context::CODE_MODE_OUTPUT_REPLACEMENT_HEADER as UNTRUSTED_REPLACEMENT_HEADER;

/// Trusted continuation guidance appended after a selected replacement.
///
/// Keeping this outside the untrusted-data fence makes the reduction semantics actionable at the
/// decision point where the parent model chooses its next cell. The guidance is Codex-owned and
/// bounded independently of the host replacement budget, like the script-status header.
/// Contextual identifiers a reducer needs to file and later serve the preserved output.
#[derive(Debug, Clone, Serialize)]
pub(super) struct ReductionContext {
    pub thread_id: String,
    pub turn_id: String,
    pub call_id: String,
    pub cell_id: String,
    /// Source of the script whose output is being reduced, when it is still
    /// known. `None` rather than empty so a host can tell "no script recorded"
    /// from "script was empty".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
    /// Bounded visible narration preceding the outer Code Mode call. Serialized
    /// explicitly in the reducer request below.
    #[serde(skip)]
    pub parent_intent: Option<String>,
    /// Bounded process continuation state derived from Codex-owned nested tool
    /// results. Serialized explicitly in the reducer request below.
    #[serde(skip)]
    pub actionable_state: Option<ActionableState>,
    /// The `Script completed` / `Script running with cell ID ...` line, before it is prepended.
    pub script_status: String,
}

/// Identity carried by a direct PostToolUse replacement acceptance callback.
pub(crate) struct PostToolUseAcceptanceContext<'a> {
    pub(crate) response_id: &'a str,
    pub(crate) session_id: &'a str,
    pub(crate) turn_id: &'a str,
    pub(crate) tool_use_id: &'a str,
}

/// Replaces code-mode output on its way into model context.
pub(super) trait CodeModeOutputReducer: Send + Sync {
    /// Returns a bounded replacement, or `None` to keep the original and truncate it.
    ///
    /// Implementations must not return `Err`; every failure mode is a `None` so callers cannot
    /// accidentally propagate a reducer outage into the turn.
    fn reduce<'a>(
        &'a self,
        context: &'a ReductionContext,
        items: &'a [FunctionCallOutputContentItem],
        max_output_tokens: usize,
    ) -> BoxFuture<'a, Option<Vec<FunctionCallOutputContentItem>>>;

    /// Trusted guidance to append after a selected replacement, when configured.
    fn continuation_guidance(&self) -> Option<&str> {
        None
    }

    /// Acknowledges a direct PostToolUse replacement after it is selected.
    fn accept_post_tool_use<'a>(
        &'a self,
        _context: PostToolUseAcceptanceContext<'a>,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}

/// Clamps the model-supplied output budget with the host-configured ceiling.
///
/// `exec` and `wait` take `max_output_tokens` / `max_tokens` straight from the model, so without a
/// ceiling a model can request a budget large enough to make any reduction pointless. Returns the
/// caller's resolved value after applying the unconditional model-context item cap.
pub(super) fn clamp_max_output_tokens(
    requested: Option<usize>,
    ceiling: Option<usize>,
) -> Option<usize> {
    let bounded = resolve_max_tokens(requested).min(MAX_MODEL_CONTEXT_ITEM_TOKENS);
    Some(ceiling.map_or(bounded, |ceiling| bounded.min(ceiling)))
}

/// The reduction seam. Called for every `RuntimeResponse` arm by `handle_runtime_response`.
pub(super) async fn apply_output_reduction(
    reducer: Option<&Arc<dyn CodeModeOutputReducer>>,
    context: &ReductionContext,
    items: Vec<FunctionCallOutputContentItem>,
    max_output_tokens: Option<usize>,
    ceiling: Option<usize>,
) -> Vec<FunctionCallOutputContentItem> {
    let max_output_tokens = clamp_max_output_tokens(max_output_tokens, ceiling);
    let actionable_output_items = match context.actionable_state.as_ref() {
        Some(actionable_state) => {
            let Some(output_items) = actionable_state.output_items() else {
                tracing::warn!(
                    "Codex-owned actionable state was not renderable; rejecting reduction"
                );
                return super::truncate_code_mode_result(items, max_output_tokens);
            };
            output_items
        }
        None => Vec::new(),
    };
    let Some(reducer) = reducer else {
        return truncate_and_append_actionable_state(
            items,
            max_output_tokens,
            actionable_output_items,
        );
    };
    let budget = resolve_max_tokens(max_output_tokens);
    let replacement = reducer.reduce(context, &items, budget).await;
    let Some(replacement) = replacement else {
        return truncate_and_append_actionable_state(
            items,
            max_output_tokens,
            actionable_output_items,
        );
    };
    // The budget applies to the host replacement. Codex-owned framing remains exact and is
    // reported separately to the host as model-visible overhead.
    let mut reduced = fence_replacement(super::truncate_code_mode_result(
        replacement,
        max_output_tokens,
    ));
    if let Some(guidance) = reducer
        .continuation_guidance()
        .and_then(CodeModeOutputReductionGuidance::new)
    {
        reduced.push(FunctionCallOutputContentItem::InputText {
            text: guidance.render(),
        });
    }
    reduced.extend(actionable_output_items);
    reduced
}

fn truncate_and_append_actionable_state(
    items: Vec<FunctionCallOutputContentItem>,
    max_output_tokens: Option<usize>,
    actionable_output_items: Vec<FunctionCallOutputContentItem>,
) -> Vec<FunctionCallOutputContentItem> {
    let mut output = super::truncate_code_mode_result(items, max_output_tokens);
    output.extend(actionable_output_items);
    output
}

/// Wraps a replacement so the parent model reads it as data rather than as instructions.
fn fence_replacement(
    replacement: Vec<FunctionCallOutputContentItem>,
) -> Vec<FunctionCallOutputContentItem> {
    let mut fenced = Vec::with_capacity(replacement.len() + 2);
    fenced.push(CodeModeOutputReplacementFence::opening().into_output_item());
    fenced.extend(replacement);
    fenced.push(CodeModeOutputReplacementFence::closing().into_output_item());
    fenced
}

/// Descriptor the host writes (mode `0600`) so Codex can find and authenticate to the reducer.
#[derive(Debug)]
struct ReducerDescriptor {
    version: u32,
    url: Url,
    token: String,
    acceptance_url: Url,
}

#[derive(Debug, Deserialize)]
struct ReducerDescriptorWire {
    version: u32,
    url: String,
    token: String,
    acceptance_url: String,
}

#[derive(Debug, Serialize)]
struct ReductionRequest<'a> {
    version: u32,
    #[serde(flatten)]
    context: &'a ReductionContext,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_intent: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    actionable_state: Option<&'a ActionableState>,
    model_visible_overhead_characters: usize,
    max_output_tokens: usize,
    content_items: &'a [FunctionCallOutputContentItem],
}

#[derive(Debug, Deserialize)]
struct ReductionResponse {
    /// `null` or absent means "no replacement"; the caller keeps the original.
    #[serde(default)]
    replacement: Option<Vec<FunctionCallOutputContentItem>>,
    /// Stable host identity for this staged gate.
    #[serde(default)]
    response_id: Option<String>,
    /// Hosts must echo the request envelope exactly. Keeping this
    /// as JSON preserves absence/null and catches any field-level conflict.
    #[serde(default)]
    actionable_state: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct ReductionAcceptance<'a> {
    version: u32,
    response_id: &'a str,
    thread_id: &'a str,
    turn_id: &'a str,
    call_id: &'a str,
    cell_id: &'a str,
}

#[derive(Debug, Serialize)]
struct PostToolUseAcceptance<'a> {
    version: u32,
    response_id: &'a str,
    session_id: &'a str,
    turn_id: &'a str,
    tool_use_id: &'a str,
}

/// Loopback HTTP reducer.
///
/// This call originates in the Codex process, which is the *parent* of the `sandbox-exec` and
/// landlock children — it is not itself sandboxed. So loopback works here even though it does not
/// work from a sandboxed child, and nothing about this path needs a sandbox hole.
pub(super) struct HttpCodeModeOutputReducer {
    config: CodeModeOutputReducerConfig,
    client: HttpClient,
}

impl HttpCodeModeOutputReducer {
    pub(super) fn new(config: CodeModeOutputReducerConfig) -> Option<Self> {
        // `build_direct` deliberately bypasses outbound proxy discovery, which is what a loopback
        // callback wants; routing this through a system proxy would be wrong and slow.
        let client = HttpClientBuilder::new()
            .connect_timeout(config.connect_timeout)
            .without_request_logging()
            .without_redirects()
            .build_direct()
            .inspect_err(|error| {
                tracing::warn!(
                    %error,
                    "code-mode output reducer disabled: HTTP client setup failed"
                );
            })
            .ok()?;
        Some(Self { config, client })
    }

    async fn post_acceptance<T: Serialize + ?Sized>(
        &self,
        descriptor: &ReducerDescriptor,
        acceptance_url: &Url,
        response_id: &str,
        acceptance: &T,
        deadline: tokio::time::Instant,
    ) {
        for attempt in 1..=ACCEPTANCE_MAX_ATTEMPTS {
            let callback = self
                .client
                .post(acceptance_url.clone())
                .bearer_auth(&descriptor.token)
                .json(acceptance)
                .send();
            match tokio::time::timeout_at(deadline, callback).await {
                Ok(Ok(response)) if response.status().is_success() => return,
                Ok(Ok(response)) => tracing::warn!(
                    status = %response.status(),
                    response_id,
                    attempt,
                    "output replacement acceptance callback returned an error status"
                ),
                Ok(Err(error)) => tracing::warn!(
                    %error,
                    response_id,
                    attempt,
                    "output replacement acceptance callback failed"
                ),
                Err(_elapsed) => {
                    tracing::warn!(
                        timeout = ?self.config.timeout,
                        response_id,
                        attempt,
                        "output replacement acceptance callback timed out"
                    );
                    return;
                }
            }
            if attempt < ACCEPTANCE_MAX_ATTEMPTS {
                let retry_at = tokio::time::Instant::now() + ACCEPTANCE_RETRY_DELAY;
                if retry_at >= deadline {
                    break;
                }
                tokio::time::sleep_until(retry_at).await;
            }
        }
        tracing::warn!(
            response_id,
            attempts = ACCEPTANCE_MAX_ATTEMPTS,
            "output replacement acceptance callback exhausted retries"
        );
    }

    /// Returns `None` for every failure so the caller falls back to truncation.
    async fn try_reduce(
        &self,
        context: &ReductionContext,
        items: &[FunctionCallOutputContentItem],
        max_output_tokens: usize,
    ) -> Option<Vec<FunctionCallOutputContentItem>> {
        let payload_bytes = estimate_payload_bytes(items);
        if payload_bytes < self.config.min_trigger_bytes {
            return None;
        }
        if payload_bytes > self.config.max_request_bytes {
            tracing::debug!(
                payload_bytes,
                limit = self.config.max_request_bytes,
                "code-mode output reducer skipped: payload above max_request_bytes"
            );
            return None;
        }

        let descriptor = read_descriptor(&self.config.descriptor_path).await?;
        let request = ReductionRequest {
            version: descriptor.version,
            context,
            parent_intent: context.parent_intent.as_deref(),
            actionable_state: context.actionable_state.as_ref(),
            model_visible_overhead_characters: model_visible_overhead_characters(
                self.continuation_guidance(),
            ),
            max_output_tokens,
            content_items: items,
        };
        let request_body = serialize_json_bounded(&request, self.config.max_request_bytes)
            .inspect_err(|error| {
                tracing::debug!(
                    %error,
                    limit = self.config.max_request_bytes,
                    "code-mode output reducer skipped: serialized request above max_request_bytes"
                );
            })
            .ok()?;

        // One budget for reduction plus the acceptance callback. Splitting it per
        // stage would let a slow reducer spend the timeout more than once.
        let reduction_started_at = tokio::time::Instant::now();
        let deadline = tokio::time::Instant::now() + self.config.timeout;
        tracing::debug!(
            call_id = %context.call_id,
            cell_id = %context.cell_id,
            script_status = %context.script_status,
            payload_bytes,
            request_bytes = request_body.len(),
            timeout_ms = self.config.timeout.as_millis(),
            "code-mode output reducer request started"
        );
        let body = tokio::time::timeout_at(deadline, async {
            let mut response = self
                .client
                .post(descriptor.url.clone())
                .bearer_auth(&descriptor.token)
                .header("content-type", "application/json")
                .body(request_body)
                .send()
                .await
                .inspect_err(|error| {
                    tracing::warn!(%error, "code-mode output reducer unreachable");
                })
                .ok()?;

            let status = response.status();
            tracing::debug!(
                call_id = %context.call_id,
                cell_id = %context.cell_id,
                %status,
                elapsed_ms = reduction_started_at.elapsed().as_millis(),
                "code-mode output reducer response headers received"
            );
            if !status.is_success() {
                tracing::warn!(%status, "code-mode output reducer returned an error status");
                return None;
            }
            // The advertised length is only an early out; the read below is the real bound.
            if response
                .content_length()
                .is_some_and(|length| length > self.config.max_response_bytes as u64)
            {
                tracing::warn!("code-mode output reducer response exceeded max_response_bytes");
                return None;
            }

            let mut body = Vec::with_capacity(
                response
                    .content_length()
                    .and_then(|length| usize::try_from(length).ok())
                    .unwrap_or_default()
                    .min(self.config.max_response_bytes),
            );
            loop {
                let chunk = response
                    .chunk()
                    .await
                    .inspect_err(|error| {
                        tracing::warn!(
                            %error,
                            "code-mode output reducer response could not be read"
                        );
                    })
                    .ok()?;
                let Some(chunk) = chunk else {
                    return Some(body);
                };
                if chunk.len() > self.config.max_response_bytes.saturating_sub(body.len()) {
                    tracing::warn!(
                        body_bytes = body.len().saturating_add(chunk.len()),
                        limit = self.config.max_response_bytes,
                        "code-mode output reducer response exceeded max_response_bytes"
                    );
                    return None;
                }
                body.extend_from_slice(&chunk);
            }
        })
        .await
        .unwrap_or_else(|_elapsed| {
            tracing::warn!(
                timeout = ?self.config.timeout,
                call_id = %context.call_id,
                cell_id = %context.cell_id,
                elapsed_ms = reduction_started_at.elapsed().as_millis(),
                "code-mode output reducer timed out; falling back to truncation"
            );
            None
        })?;
        tracing::debug!(
            call_id = %context.call_id,
            cell_id = %context.cell_id,
            response_bytes = body.len(),
            elapsed_ms = reduction_started_at.elapsed().as_millis(),
            "code-mode output reducer response completed"
        );

        let parsed = serde_json::from_slice::<ReductionResponse>(&body)
            .inspect_err(|error| {
                tracing::warn!(%error, "code-mode output reducer returned a malformed response");
            })
            .ok()?;
        let replacement = parsed.replacement?;
        // An empty replacement would hand the model a bare fence with nothing in it.
        if replacement.is_empty() {
            tracing::warn!("code-mode output reducer returned an empty replacement");
            return None;
        }
        if !replacement
            .iter()
            .all(|item| matches!(item, FunctionCallOutputContentItem::InputText { .. }))
        {
            tracing::warn!("code-mode output reducer returned a non-text replacement");
            return None;
        }
        let expected_actionable_state = context
            .actionable_state
            .as_ref()
            .map(ActionableState::to_json_value);
        if parsed.actionable_state != expected_actionable_state {
            tracing::warn!(
                "code-mode output reducer response lost or conflicted with actionable state"
            );
            return None;
        }
        let response_id = parsed
            .response_id
            .as_deref()
            .and_then(|response_id| (!response_id.trim().is_empty()).then_some(response_id))
            .or_else(|| {
                tracing::warn!("code-mode output reducer response omitted a non-empty response_id");
                None
            })?;
        let acceptance = ReductionAcceptance {
            version: REDUCER_PROTOCOL_VERSION,
            response_id,
            thread_id: &context.thread_id,
            turn_id: &context.turn_id,
            call_id: &context.call_id,
            cell_id: &context.cell_id,
        };
        // This is a one-way commit notification. Once the request is attempted, Codex must
        // keep the replacement even if every bounded attempt is lost: the host may already
        // have received one and finalized its staged gate.
        self.post_acceptance(
            &descriptor,
            &descriptor.acceptance_url,
            response_id,
            &acceptance,
            deadline,
        )
        .await;
        tracing::debug!(
            call_id = %context.call_id,
            cell_id = %context.cell_id,
            response_id,
            elapsed_ms = reduction_started_at.elapsed().as_millis(),
            "code-mode output reducer replacement selected and acceptance attempted"
        );
        Some(replacement)
    }

    async fn try_accept_post_tool_use(&self, context: PostToolUseAcceptanceContext<'_>) {
        let Some(descriptor) = read_descriptor(&self.config.descriptor_path).await else {
            return;
        };
        if descriptor.version != REDUCER_PROTOCOL_VERSION {
            return;
        }
        let acceptance = PostToolUseAcceptance {
            version: REDUCER_PROTOCOL_VERSION,
            response_id: context.response_id,
            session_id: context.session_id,
            turn_id: context.turn_id,
            tool_use_id: context.tool_use_id,
        };
        self.post_acceptance(
            &descriptor,
            &descriptor.acceptance_url,
            context.response_id,
            &acceptance,
            tokio::time::Instant::now() + self.config.timeout,
        )
        .await;
    }
}

impl CodeModeOutputReducer for HttpCodeModeOutputReducer {
    fn reduce<'a>(
        &'a self,
        context: &'a ReductionContext,
        items: &'a [FunctionCallOutputContentItem],
        max_output_tokens: usize,
    ) -> BoxFuture<'a, Option<Vec<FunctionCallOutputContentItem>>> {
        Box::pin(self.try_reduce(context, items, max_output_tokens))
    }

    fn continuation_guidance(&self) -> Option<&str> {
        self.config.continuation_guidance.as_deref()
    }

    fn accept_post_tool_use<'a>(
        &'a self,
        context: PostToolUseAcceptanceContext<'a>,
    ) -> BoxFuture<'a, ()> {
        Box::pin(self.try_accept_post_tool_use(context))
    }
}

fn model_visible_overhead_characters(continuation_guidance: Option<&str>) -> usize {
    CodeModeOutputReplacementFence::opening()
        .render()
        .chars()
        .count()
        + CodeModeOutputReplacementFence::closing()
            .render()
            .chars()
            .count()
        + continuation_guidance.map_or(0, |guidance| {
            guidance
                .chars()
                .take(crate::config::CODE_MODE_REDUCER_GUIDANCE_MAX_CHARACTERS)
                .count()
        })
}

/// Re-read per reduction so the host can restart and rotate its token without restarting Codex.
async fn read_descriptor(path: &Path) -> Option<ReducerDescriptor> {
    let contents = tokio::fs::read(path)
        .await
        .inspect_err(|error| {
            tracing::debug!(
                path = %path.display(),
                %error,
                "code-mode output reducer descriptor unavailable"
            );
        })
        .ok()?;
    let descriptor = serde_json::from_slice::<ReducerDescriptorWire>(&contents)
        .inspect_err(|error| {
            tracing::warn!(%error, "code-mode output reducer descriptor is malformed");
        })
        .ok()?;
    if descriptor.version != REDUCER_PROTOCOL_VERSION {
        tracing::warn!(
            version = descriptor.version,
            expected = REDUCER_PROTOCOL_VERSION,
            "code-mode output reducer descriptor version is unsupported"
        );
        return None;
    }
    let url = parse_loopback_endpoint(&descriptor.url, "url")?;
    let acceptance_url = parse_loopback_endpoint(&descriptor.acceptance_url, "acceptance_url")?;
    Some(ReducerDescriptor {
        version: descriptor.version,
        url,
        token: descriptor.token,
        acceptance_url,
    })
}

fn parse_loopback_endpoint(endpoint: &str, field: &str) -> Option<Url> {
    let url = Url::parse(endpoint)
        .inspect_err(|error| {
            tracing::warn!(%error, field, "code-mode output reducer endpoint is malformed");
        })
        .ok()?;
    let is_loopback = match url.host() {
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        Some(Host::Domain(_)) | None => false,
    };
    if url.scheme() != "http"
        || !is_loopback
        || !url.username().is_empty()
        || url.password().is_some()
    {
        tracing::warn!(
            field,
            endpoint,
            "code-mode output reducer endpoint is not loopback HTTP"
        );
        return None;
    }
    Some(url)
}

/// Cheap size proxy for the gate. Exact serialized length is not worth computing per call.
fn estimate_payload_bytes(items: &[FunctionCallOutputContentItem]) -> usize {
    items
        .iter()
        .map(|item| match item {
            FunctionCallOutputContentItem::InputText { text } => text.len(),
            FunctionCallOutputContentItem::InputImage { image_url, .. } => image_url.len(),
            FunctionCallOutputContentItem::InputAudio { audio_url } => audio_url.len(),
            FunctionCallOutputContentItem::EncryptedContent { encrypted_content } => {
                encrypted_content.len()
            }
        })
        .sum()
}

fn serialize_json_bounded<T: Serialize>(value: &T, limit: usize) -> io::Result<Vec<u8>> {
    let mut writer = BoundedWriter {
        bytes: Vec::with_capacity(limit.min(64 * 1024)),
        limit,
    };
    serde_json::to_writer(&mut writer, value).map_err(io::Error::other)?;
    Ok(writer.bytes)
}

struct BoundedWriter {
    bytes: Vec<u8>,
    limit: usize,
}

impl Write for BoundedWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "serialized reducer request exceeded max_request_bytes",
            ));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
#[path = "reducer_tests.rs"]
mod tests;
