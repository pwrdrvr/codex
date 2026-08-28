mod actionable_state;
mod delegate;
mod execute_handler;
pub(crate) mod execute_spec;
pub(crate) mod pwrdrvr_token_miser;
mod reducer;
mod response_adapter;
mod telemetry;
mod wait_handler;
pub(crate) mod wait_spec;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use codex_code_mode::CellId;
use codex_code_mode::CodeModeNestedToolCall;
use codex_code_mode::CodeModeSession;
use codex_code_mode::CodeModeSessionProvider;
use codex_code_mode::CodeModeToolKind;
use codex_code_mode::RuntimeResponse;
use codex_protocol::models::FunctionCallOutputContentItem;
use futures::future::join_all;
use serde_json::Value as JsonValue;
use tokio::sync::OnceCell;
use tokio_util::sync::CancellationToken;

use crate::config::CodeModeConfig;
use crate::function_tool::FunctionCallError;
use crate::original_image_detail::can_request_original_image_detail;
use crate::original_image_detail::sanitize_original_image_detail as sanitize_image_detail_items;
use crate::session::session::Session;
use crate::session::step_context::StepContext;
use crate::session::turn_context::TurnContext;
use crate::tools::ExecutedToolCallRecorder;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::SharedTurnDiffTracker;
use crate::tools::context::ToolPayload;
use crate::tools::parallel::ToolCallRuntime;
use crate::tools::router::ToolCall;
use crate::tools::router::ToolCallSource;
use crate::unified_exec::resolve_max_tokens;
use codex_protocol::openai_models::ToolMode;
use codex_tools::ToolName;
use codex_utils_audio::estimate_audio_token_count;
use codex_utils_output_truncation::TruncationPolicy;
use codex_utils_output_truncation::formatted_truncate_text_content_items_with_policy;
use codex_utils_output_truncation::truncate_function_output_items_with_policy;

use actionable_state::ActionableStateStore;
use delegate::CodeModeDispatchBroker;
use delegate::CodeModeDispatchWorker;
pub(crate) use execute_handler::CodeModeExecuteHandler;
use reducer::CodeModeOutputReducer;
use reducer::HttpCodeModeOutputReducer;
pub(crate) use reducer::PostToolUseAcceptanceContext;
use reducer::ReductionContext;
use reducer::apply_output_reduction;
use response_adapter::into_function_call_output_content_items;
pub(crate) use wait_handler::CodeModeWaitHandler;

pub(crate) const PUBLIC_TOOL_NAME: &str = codex_code_mode::PUBLIC_TOOL_NAME;
pub(crate) const WAIT_TOOL_NAME: &str = codex_code_mode::WAIT_TOOL_NAME;
pub(crate) const DEFAULT_WAIT_YIELD_TIME_MS: u64 = codex_code_mode::DEFAULT_WAIT_YIELD_TIME_MS;

/// Returns true for the code-mode `exec` tool in the default namespace.
pub(crate) fn is_exec_tool_name(tool_name: &ToolName) -> bool {
    tool_name.is_default_namespace() && tool_name.name == PUBLIC_TOOL_NAME
}

#[derive(Clone)]
pub(crate) struct ExecContext {
    pub(super) session: Arc<Session>,
    pub(super) turn: Arc<TurnContext>,
}

pub(crate) struct CodeModeService {
    session: OnceCell<Arc<dyn CodeModeSession>>,
    session_provider: Arc<dyn CodeModeSessionProvider>,
    availability: Result<(), String>,
    dispatch_broker: Arc<CodeModeDispatchBroker>,
    default_exec_yield_time_ms: u64,
    shutdown_token: CancellationToken,
    /// Runtime-refreshable settings for the script-to-model reduction boundary.
    reduction_config: RwLock<CodeModeReductionRuntimeConfig>,
    /// Script source per live cell, so a reduction can tell the host what
    /// produced the output it is summarizing. `wait` resumes a cell it did not
    /// start, so the source has to outlive the `exec` call that carried it.
    /// Only populated when a reducer is configured.
    cell_scripts: Mutex<HashMap<CellId, Arc<str>>>,
    /// Bounded visible narration keyed by a direct model tool call until the
    /// call starts or reaches PostToolUse.
    direct_parent_intents: Mutex<HashMap<String, Arc<str>>>,
    /// The outer `exec` narration inherited by reducer requests and nested
    /// PostToolUse calls for the lifetime of a Code Mode cell.
    cell_parent_intents: Mutex<HashMap<CellId, Arc<str>>>,
    /// Codex-owned process handles observed in nested unified-exec results.
    /// Reducers can summarize the accompanying output but cannot replace this
    /// state without echoing it exactly.
    actionable_states: ActionableStateStore,
    pwrdrvr_token_miser: pwrdrvr_token_miser::PwrdrvrTokenMiserGate,
    unavailable_warning_emitted: AtomicBool,
}

#[derive(Clone, Default)]
struct CodeModeReductionRuntimeConfig {
    max_output_tokens_ceiling: Option<usize>,
    output_reducer: Option<Arc<dyn CodeModeOutputReducer>>,
}

impl CodeModeReductionRuntimeConfig {
    fn from_config(config: &CodeModeConfig) -> Self {
        let output_reducer = config
            .output_reducer
            .clone()
            .and_then(HttpCodeModeOutputReducer::new)
            .map(|reducer| Arc::new(reducer) as Arc<dyn CodeModeOutputReducer>);
        Self {
            max_output_tokens_ceiling: config.max_output_tokens_ceiling,
            output_reducer,
        }
    }
}

impl CodeModeService {
    pub(crate) fn new(
        session_provider: Arc<dyn CodeModeSessionProvider>,
        config: &CodeModeConfig,
        executed_tool_calls: Option<Arc<ExecutedToolCallRecorder>>,
    ) -> Self {
        let dispatch_broker = Arc::new(CodeModeDispatchBroker::new(executed_tool_calls));
        let availability = session_provider.availability();
        Self {
            session: OnceCell::new(),
            session_provider,
            availability,
            dispatch_broker,
            default_exec_yield_time_ms: config.default_exec_yield_time_ms,
            shutdown_token: CancellationToken::new(),
            reduction_config: RwLock::new(CodeModeReductionRuntimeConfig::from_config(config)),
            cell_scripts: Mutex::new(HashMap::new()),
            direct_parent_intents: Mutex::new(HashMap::new()),
            cell_parent_intents: Mutex::new(HashMap::new()),
            actionable_states: ActionableStateStore::default(),
            pwrdrvr_token_miser: pwrdrvr_token_miser::PwrdrvrTokenMiserGate::new(),
            unavailable_warning_emitted: AtomicBool::new(false),
        }
    }

    pub(crate) async fn accept_post_tool_use_replacements(
        &self,
        response_ids: &[String],
        session_id: &str,
        turn_id: &str,
        tool_use_id: &str,
    ) {
        let reducer = self.reduction_config().output_reducer;
        if let Some(reducer) = reducer {
            join_all(response_ids.iter().map(|response_id| {
                reducer.accept_post_tool_use(PostToolUseAcceptanceContext {
                    response_id,
                    session_id,
                    turn_id,
                    tool_use_id,
                })
            }))
            .await;
        }
    }

    pub(crate) fn set_pwrdrvr_token_miser_activation_nonce(
        &self,
        activation_nonce: Option<Arc<str>>,
    ) {
        self.pwrdrvr_token_miser
            .set_activation_nonce(activation_nonce);
    }

    pub(crate) fn pwrdrvr_token_miser_is_enabled(&self) -> bool {
        self.pwrdrvr_token_miser.is_enabled()
    }

    pub(crate) async fn run_pwrdrvr_token_miser(
        &self,
        request: &codex_hooks::PostToolUseRequest,
    ) -> Option<pwrdrvr_token_miser::ManagedPostToolUseReplacement> {
        self.pwrdrvr_token_miser.run(request).await
    }

    pub(crate) async fn accept_pwrdrvr_token_miser_replacement(
        &self,
        acceptance: &pwrdrvr_token_miser::ManagedPostToolUseAcceptance,
        session_id: &str,
        turn_id: &str,
        tool_use_id: &str,
    ) {
        self.pwrdrvr_token_miser
            .accept(acceptance, session_id, turn_id, tool_use_id)
            .await;
    }

    pub(crate) fn record_actionable_tool_result(
        &self,
        source: &ToolCallSource,
        tool_name: &ToolName,
        payload: &ToolPayload,
        result: &JsonValue,
    ) {
        let ToolCallSource::CodeMode { cell_id, .. } = source else {
            return;
        };
        let input = match payload {
            ToolPayload::Function { arguments } => serde_json::from_str(arguments).ok(),
            ToolPayload::Custom { input } => Some(JsonValue::String(input.clone())),
            ToolPayload::ToolSearch { .. } => None,
        };
        self.actionable_states.record(
            &CellId::new(cell_id.clone()),
            tool_name,
            input.as_ref(),
            result,
        );
    }

    pub(crate) fn is_available(&self) -> bool {
        self.availability.is_ok()
    }

    fn reduction_config(&self) -> CodeModeReductionRuntimeConfig {
        self.reduction_config
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn refresh_reduction_config(&self, config: &CodeModeConfig) {
        let next = CodeModeReductionRuntimeConfig::from_config(config);
        if next.output_reducer.is_none()
            && let Ok(mut scripts) = self.cell_scripts.lock()
        {
            scripts.clear();
        }
        *self
            .reduction_config
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = next;
    }

    /// Remembers what a cell is running so the reducer can be told. Skipped
    /// entirely when no reducer is configured, so the default path allocates
    /// nothing.
    pub(crate) fn record_cell_script(&self, cell_id: &CellId, source: &str) {
        if self.reduction_config().output_reducer.is_none() {
            return;
        }
        if let Ok(mut scripts) = self.cell_scripts.lock() {
            scripts.insert(cell_id.clone(), Arc::from(source));
        }
    }

    pub(crate) fn record_direct_parent_intent(
        &self,
        call_id: &str,
        parent_intent: Option<Arc<str>>,
    ) {
        let Some(parent_intent) = parent_intent else {
            return;
        };
        if let Ok(mut intents) = self.direct_parent_intents.lock() {
            intents.insert(call_id.to_string(), parent_intent);
        }
    }

    pub(crate) fn discard_direct_parent_intent(&self, call_id: &str) {
        if let Ok(mut intents) = self.direct_parent_intents.lock() {
            intents.remove(call_id);
        }
    }

    pub(crate) fn move_parent_intent_to_cell(&self, call_id: &str, cell_id: &CellId) {
        let parent_intent = self
            .direct_parent_intents
            .lock()
            .ok()
            .and_then(|mut intents| intents.remove(call_id));
        if let Some(parent_intent) = parent_intent
            && let Ok(mut intents) = self.cell_parent_intents.lock()
        {
            intents.insert(cell_id.clone(), parent_intent);
        }
    }

    pub(crate) fn take_parent_intent(
        &self,
        call_id: &str,
        source: &ToolCallSource,
    ) -> Option<String> {
        let parent_intent = match source {
            ToolCallSource::Direct | ToolCallSource::DirectPlaintextMessage => {
                self.direct_parent_intents.lock().ok()?.remove(call_id)
            }
            ToolCallSource::CodeMode { cell_id, .. } => self
                .cell_parent_intents
                .lock()
                .ok()?
                .get(&CellId::new(cell_id.clone()))
                .cloned(),
        }?;
        Some(parent_intent.to_string())
    }

    /// Reads the script for a cell, removing it once the cell can produce no
    /// more output. A cell that errors before reaching `handle_runtime_response`
    /// leaves its entry behind until the session ends; the entry is one script
    /// source, and `interrupt_active_cells` clears the map wholesale.
    fn cell_script(&self, cell_id: &CellId, take: bool) -> Option<Arc<str>> {
        let mut scripts = self.cell_scripts.lock().ok()?;
        if take {
            scripts.remove(cell_id)
        } else {
            scripts.get(cell_id).cloned()
        }
    }

    pub(crate) fn take_unavailable_warning(&self, tool_mode: ToolMode) -> Option<String> {
        let error = self.availability.as_ref().err()?;
        let behavior = match tool_mode {
            ToolMode::Direct => "Falling back to direct tools",
            ToolMode::CodeMode | ToolMode::CodeModeOnly => "Code mode will fail closed",
        };
        (!self
            .unavailable_warning_emitted
            .swap(true, Ordering::Relaxed))
        .then(|| {
            format!(
                "Code Mode is unavailable because {error}. {behavior}; enable `features.code_mode_host` and install `codex-code-mode-host`."
            )
        })
    }

    pub(crate) fn session_provider(&self) -> Arc<dyn CodeModeSessionProvider> {
        Arc::clone(&self.session_provider)
    }

    pub(crate) async fn execute(
        &self,
        mut request: codex_code_mode::ExecuteRequest,
    ) -> Result<codex_code_mode::StartedCell, String> {
        request
            .yield_time_ms
            .get_or_insert(self.default_exec_yield_time_ms);
        self.session().await?.execute(request).await
    }

    pub(crate) async fn wait(
        &self,
        request: codex_code_mode::WaitRequest,
    ) -> Result<codex_code_mode::WaitOutcome, String> {
        self.session().await?.wait(request).await
    }

    pub(crate) async fn terminate(
        &self,
        cell_id: CellId,
    ) -> Result<codex_code_mode::WaitOutcome, String> {
        self.session().await?.terminate(cell_id).await
    }

    pub(crate) async fn interrupt_active_cells(&self) {
        if let Ok(mut scripts) = self.cell_scripts.lock() {
            scripts.clear();
        }
        if let Ok(mut intents) = self.direct_parent_intents.lock() {
            intents.clear();
        }
        if let Ok(mut intents) = self.cell_parent_intents.lock() {
            intents.clear();
        }
        self.actionable_states.clear();
        let Some(session) = self.session.get() else {
            return;
        };
        join_all(
            self.dispatch_broker
                .active_cell_ids()
                .into_iter()
                .map(|cell_id| async move {
                    if let Err(error) = session.terminate(cell_id.clone()).await {
                        tracing::warn!(%cell_id, %error, "failed to terminate interrupted code-mode cell");
                    }
                }),
        )
        .await;
    }

    pub(crate) async fn shutdown(&self) -> Result<(), String> {
        self.shutdown_token.cancel();
        // Join any initialization already in progress without initializing an unused service.
        match self
            .session
            .get_or_try_init(|| async {
                Err::<Arc<dyn CodeModeSession>, String>(
                    "code mode session is shutting down".to_string(),
                )
            })
            .await
        {
            Ok(session) => session.shutdown().await,
            Err(_) => Ok(()),
        }
    }

    pub(crate) fn mark_cell_ready_for_dispatch(
        &self,
        cell_id: &codex_code_mode::CellId,
        originating_item_id: Option<codex_protocol::ResponseItemId>,
    ) {
        self.dispatch_broker
            .mark_cell_ready_for_dispatch(cell_id, originating_item_id);
    }

    pub(crate) fn cell_originating_item_id(
        &self,
        cell_id: &codex_code_mode::CellId,
    ) -> Option<codex_protocol::ResponseItemId> {
        self.dispatch_broker.cell_originating_item_id(cell_id)
    }

    pub(crate) fn finish_cell_dispatch(&self, cell_id: &CellId) {
        self.dispatch_broker.close_cell(cell_id);
    }

    pub(crate) fn start_turn_worker(
        &self,
        session: &Arc<Session>,
        step_context: Arc<StepContext>,
        tracker: SharedTurnDiffTracker,
    ) -> Option<CodeModeDispatchWorker> {
        let turn = &step_context.turn;
        if !step_context.tool_router.requires_code_mode_worker() {
            return None;
        }

        let exec = ExecContext {
            session: Arc::clone(session),
            turn: Arc::clone(turn),
        };
        Some(
            self.dispatch_broker
                .start_turn_worker(exec, step_context, tracker),
        )
    }

    pub(crate) async fn session(&self) -> Result<Arc<dyn CodeModeSession>, String> {
        if self.shutdown_token.is_cancelled() {
            return Err("code mode session is shutting down".to_string());
        }
        self.session
            .get_or_try_init(|| async {
                if self.shutdown_token.is_cancelled() {
                    return Err("code mode session is shutting down".to_string());
                }
                let session = tokio::select! {
                    biased;
                    _ = self.shutdown_token.cancelled() => {
                        return Err("code mode session is shutting down".to_string());
                    }
                    session = self
                        .session_provider
                        .create_session(self.dispatch_broker.clone()) => session?,
                };
                if self.shutdown_token.is_cancelled() {
                    let _ = session.shutdown().await;
                    return Err("code mode session is shutting down".to_string());
                }
                Ok(session)
            })
            .await
            .map(Arc::clone)
    }
}

pub(super) async fn handle_runtime_response(
    exec: &ExecContext,
    call_id: &str,
    response: RuntimeResponse,
    max_output_tokens: Option<usize>,
    started_at: std::time::Instant,
) -> Result<FunctionToolOutput, String> {
    let script_status = format_script_status(&response);
    // A yielded cell can produce more output, so keep its script; anything else
    // is finished with it.
    let is_terminal = !matches!(response, RuntimeResponse::Yielded { .. });
    let service = &exec.session.services.code_mode_service;
    let reduction_config = service.reduction_config();
    let reduction_enabled = reduction_config.output_reducer.is_some();
    // Scoped so the borrow of `response` ends before the match below moves it.
    let (cell_id, script, parent_intent, actionable_state) = {
        let cell_id = response_cell_id(&response);
        let parent_intent = {
            let mut intents = service.cell_parent_intents.lock().ok();
            if is_terminal {
                intents.as_mut().and_then(|intents| intents.remove(cell_id))
            } else {
                intents
                    .as_ref()
                    .and_then(|intents| intents.get(cell_id).cloned())
            }
        };
        let script = service.cell_script(cell_id, is_terminal);
        let actionable_state = service.actionable_states.read(cell_id, is_terminal);
        let (script, parent_intent, actionable_state) = if reduction_enabled {
            (
                script.map(|script| script.to_string()),
                parent_intent.map(|parent_intent| parent_intent.to_string()),
                actionable_state,
            )
        } else {
            (None, None, None)
        };
        (cell_id.to_string(), script, parent_intent, actionable_state)
    };
    // The script-to-model boundary: everything below is what enters model context, as opposed to
    // the nested-tool result in `call_nested_tool`, which is returned into the running script.
    let context = ReductionContext {
        thread_id: exec.session.thread_id.to_string(),
        turn_id: exec.turn.sub_id.clone(),
        call_id: call_id.to_string(),
        // Summarizing a wall of text is guesswork without knowing it came from,
        // say, a `rg --files` invocation, so hand the reducer the program that
        // produced it.
        script,
        parent_intent,
        actionable_state,
        cell_id,
        script_status: script_status.clone(),
    };

    match response {
        RuntimeResponse::Yielded { content_items, .. } => {
            let mut content_items = into_function_call_output_content_items(content_items);
            sanitize_runtime_image_detail(exec.turn.as_ref(), &mut content_items);
            content_items = reduce_code_mode_result(
                &reduction_config,
                &context,
                content_items,
                max_output_tokens,
            )
            .await;
            prepend_script_status(&mut content_items, &script_status, started_at.elapsed());
            Ok(FunctionToolOutput::from_content(content_items, Some(true)))
        }
        RuntimeResponse::Terminated { content_items, .. } => {
            let mut content_items = into_function_call_output_content_items(content_items);
            sanitize_runtime_image_detail(exec.turn.as_ref(), &mut content_items);
            content_items = reduce_code_mode_result(
                &reduction_config,
                &context,
                content_items,
                max_output_tokens,
            )
            .await;
            prepend_script_status(&mut content_items, &script_status, started_at.elapsed());
            Ok(FunctionToolOutput::from_content(content_items, Some(true)))
        }
        RuntimeResponse::Result {
            content_items,
            error_text,
            ..
        } => {
            let mut content_items = into_function_call_output_content_items(content_items);
            sanitize_runtime_image_detail(exec.turn.as_ref(), &mut content_items);
            let success = error_text.is_none();
            if let Some(error_text) = error_text {
                content_items.push(FunctionCallOutputContentItem::InputText {
                    text: format!("Script error:\n{error_text}"),
                });
            }
            content_items = reduce_code_mode_result(
                &reduction_config,
                &context,
                content_items,
                max_output_tokens,
            )
            .await;
            prepend_script_status(&mut content_items, &script_status, started_at.elapsed());
            Ok(FunctionToolOutput::from_content(
                content_items,
                Some(success),
            ))
        }
    }
}

/// Applies the host reduction seam, falling back to the built-in truncation in every other case.
async fn reduce_code_mode_result(
    reduction_config: &CodeModeReductionRuntimeConfig,
    context: &ReductionContext,
    content_items: Vec<FunctionCallOutputContentItem>,
    max_output_tokens: Option<usize>,
) -> Vec<FunctionCallOutputContentItem> {
    apply_output_reduction(
        reduction_config.output_reducer.as_ref(),
        context,
        content_items,
        max_output_tokens,
        reduction_config.max_output_tokens_ceiling,
    )
    .await
}

fn response_cell_id(response: &RuntimeResponse) -> &CellId {
    match response {
        RuntimeResponse::Yielded { cell_id, .. }
        | RuntimeResponse::Terminated { cell_id, .. }
        | RuntimeResponse::Result { cell_id, .. } => cell_id,
    }
}

fn sanitize_runtime_image_detail(turn: &TurnContext, items: &mut [FunctionCallOutputContentItem]) {
    sanitize_image_detail_items(can_request_original_image_detail(turn.model_info()), items);
}

fn format_script_status(response: &RuntimeResponse) -> String {
    match response {
        RuntimeResponse::Yielded { cell_id, .. } => {
            format!("Script running with cell ID {cell_id}")
        }
        RuntimeResponse::Terminated { .. } => "Script terminated".to_string(),
        RuntimeResponse::Result { error_text, .. } => {
            if error_text.is_none() {
                "Script completed".to_string()
            } else {
                "Script failed".to_string()
            }
        }
    }
}

fn prepend_script_status(
    content_items: &mut Vec<FunctionCallOutputContentItem>,
    status: &str,
    wall_time: Duration,
) {
    let wall_time_seconds = ((wall_time.as_secs_f32()) * 10.0).round() / 10.0;
    let header = format!("{status}\nWall time {wall_time_seconds:.1} seconds\nOutput:\n");
    content_items.insert(0, FunctionCallOutputContentItem::InputText { text: header });
}

fn truncate_code_mode_result(
    items: Vec<FunctionCallOutputContentItem>,
    max_output_tokens: Option<usize>,
) -> Vec<FunctionCallOutputContentItem> {
    let max_output_tokens = resolve_max_tokens(max_output_tokens);
    let policy = TruncationPolicy::Tokens(max_output_tokens);
    if items
        .iter()
        .all(|item| matches!(item, FunctionCallOutputContentItem::InputText { .. }))
    {
        let (truncated_items, _) =
            formatted_truncate_text_content_items_with_policy(&items, policy);
        return truncated_items;
    }

    truncate_function_output_items_with_policy(&items, policy, estimate_audio_token_count)
}

// Submit synchronously so the recorder sees the call before the cell's dispatch gate closes.
fn submit_nested_tool(
    exec: ExecContext,
    tool_runtime: ToolCallRuntime,
    invocation: CodeModeNestedToolCall,
    cancellation_token: CancellationToken,
) -> Result<
    impl std::future::Future<Output = Result<JsonValue, FunctionCallError>> + Send + 'static,
    FunctionCallError,
> {
    let CodeModeNestedToolCall {
        cell_id,
        runtime_tool_call_id,
        tool_name,
        tool_kind,
        input,
    } = invocation;
    if is_exec_tool_name(&tool_name) {
        return Err(FunctionCallError::RespondToModel(format!(
            "{PUBLIC_TOOL_NAME} cannot invoke itself"
        )));
    }

    let payload = match build_nested_tool_payload(tool_kind, &tool_name, input) {
        Ok(payload) => payload,
        Err(error) => return Err(FunctionCallError::RespondToModel(error)),
    };

    let call = ToolCall {
        tool_name: tool_name.with_default_namespace(),
        call_id: format!("{PUBLIC_TOOL_NAME}-{}", uuid::Uuid::new_v4()),
        payload,
        encrypted_function_args: None,
    };
    exec.session
        .services
        .analytics_events_client
        .track_code_mode_tool_call(codex_analytics::CodeModeToolCallFact::ChildStarted {
            thread_id: exec.session.thread_id.to_string(),
            turn_id: exec.turn.sub_id.clone(),
            call_id: call.call_id.clone(),
            cell_id: cell_id.to_string(),
        });
    let result = tool_runtime.handle_tool_call_with_source(
        call,
        ToolCallSource::CodeMode {
            cell_id: cell_id.to_string(),
            runtime_tool_call_id,
        },
        cancellation_token,
    );
    Ok(async move { Ok(result.await?.code_mode_result()) })
}

fn build_nested_tool_payload(
    tool_kind: CodeModeToolKind,
    tool_name: &ToolName,
    input: Option<JsonValue>,
) -> Result<ToolPayload, String> {
    match tool_kind {
        CodeModeToolKind::Function => build_function_tool_payload(tool_name, input),
        CodeModeToolKind::Freeform => build_freeform_tool_payload(tool_name, input),
    }
}

fn build_function_tool_payload(
    tool_name: &ToolName,
    input: Option<JsonValue>,
) -> Result<ToolPayload, String> {
    let arguments = serialize_function_tool_arguments(tool_name, input)?;
    Ok(ToolPayload::Function { arguments })
}

fn serialize_function_tool_arguments(
    tool_name: &ToolName,
    input: Option<JsonValue>,
) -> Result<String, String> {
    match input {
        None => Ok("{}".to_string()),
        Some(JsonValue::Object(map)) => serde_json::to_string(&JsonValue::Object(map))
            .map_err(|err| format!("failed to serialize tool `{tool_name}` arguments: {err}")),
        Some(_) => Err(format!(
            "tool `{tool_name}` expects a JSON object for arguments"
        )),
    }
}

fn build_freeform_tool_payload(
    tool_name: &ToolName,
    input: Option<JsonValue>,
) -> Result<ToolPayload, String> {
    match input {
        Some(JsonValue::String(input)) => Ok(ToolPayload::Custom { input }),
        _ => Err(format!("tool `{tool_name}` expects a string input")),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use super::CodeModeService;
    use super::build_nested_tool_payload;
    use super::reducer::CodeModeOutputReducer;
    use super::reducer::PostToolUseAcceptanceContext;
    use super::reducer::ReductionContext;
    use super::truncate_code_mode_result;
    use crate::config::CodeModeConfig;
    use crate::config::CodeModeOutputReducerConfig;
    use crate::config::DEFAULT_CODE_MODE_REDUCER_TOOL_DESCRIPTION_GUIDANCE;
    use crate::session::step_context::StepContext;
    use crate::session::tests::make_session_and_context;
    use crate::tools::context::ToolPayload;
    use crate::tools::registry::ToolRegistry;
    use crate::tools::router::ToolRouter;
    use crate::turn_diff_tracker::TurnDiffTracker;
    use codex_code_mode::CellId;
    use codex_code_mode::CodeModeToolKind;
    use codex_code_mode::DisabledCodeModeSessionProvider;
    use codex_protocol::models::FunctionCallOutputContentItem;
    use codex_protocol::openai_models::ToolMode;
    use codex_tools::ToolName;
    use futures::future::BoxFuture;
    use serde_json::json;

    #[tokio::test]
    async fn turn_worker_uses_step_router_mode_instead_of_admitted_turn() {
        let (session, turn) = make_session_and_context().await;
        assert_eq!(
            crate::tools::effective_tool_mode(&turn, turn.model_info()),
            ToolMode::Direct
        );
        let session = Arc::new(session);
        let step_context = StepContext::for_test(Arc::new(turn));
        let router = Arc::new(ToolRouter::from_parts(
            ToolRegistry::empty_for_test(),
            Vec::new(),
            ToolMode::CodeModeOnly,
            BTreeMap::new(),
            /*tool_namespaces_info*/ None,
            &[],
        ));
        let step_context = step_context.with_tool_router_for_test(router);
        let tracker = Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new()));

        let worker =
            session
                .services
                .code_mode_service
                .start_turn_worker(&session, step_context, tracker);

        assert!(worker.is_some());
    }

    struct AcceptanceConcurrencyProbe {
        active: AtomicUsize,
        max_active: AtomicUsize,
    }

    impl CodeModeOutputReducer for AcceptanceConcurrencyProbe {
        fn reduce<'a>(
            &'a self,
            _context: &'a ReductionContext,
            _items: &'a [FunctionCallOutputContentItem],
            _max_output_tokens: usize,
        ) -> BoxFuture<'a, Option<Vec<FunctionCallOutputContentItem>>> {
            Box::pin(async { None })
        }

        fn accept_post_tool_use<'a>(
            &'a self,
            _context: PostToolUseAcceptanceContext<'a>,
        ) -> BoxFuture<'a, ()> {
            Box::pin(async move {
                let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
                self.max_active.fetch_max(active, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(50)).await;
                self.active.fetch_sub(1, Ordering::SeqCst);
            })
        }
    }

    fn reducer_config() -> CodeModeConfig {
        CodeModeConfig {
            output_reducer: Some(CodeModeOutputReducerConfig {
                descriptor_path: PathBuf::from("unused-reducer-descriptor.json"),
                min_trigger_bytes: 0,
                max_request_bytes: 1_024,
                max_response_bytes: 1_024,
                timeout: Duration::from_secs(1),
                connect_timeout: Duration::from_secs(1),
                tool_description_guidance: DEFAULT_CODE_MODE_REDUCER_TOOL_DESCRIPTION_GUIDANCE
                    .to_string(),
                continuation_guidance: None,
            }),
            ..CodeModeConfig::default()
        }
    }

    fn service(config: &CodeModeConfig) -> CodeModeService {
        CodeModeService::new(
            Arc::new(DisabledCodeModeSessionProvider),
            config,
            /*executed_tool_calls*/ None,
        )
    }

    #[tokio::test]
    async fn acceptance_callbacks_run_concurrently() {
        let service = service(&CodeModeConfig::default());
        let reducer = Arc::new(AcceptanceConcurrencyProbe {
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
        });
        service
            .reduction_config
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .output_reducer = Some(reducer.clone());

        service
            .accept_post_tool_use_replacements(
                &["response-1".to_string(), "response-2".to_string()],
                "session-id",
                "turn-id",
                "tool-use-id",
            )
            .await;

        assert_eq!(reducer.max_active.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn actionable_state_survives_reducer_disable_and_reenable() {
        let enabled = reducer_config();
        let disabled = CodeModeConfig::default();
        let service = service(&enabled);
        let cell_id = CellId::new("live-cell".to_string());
        service.actionable_states.record(
            &cell_id,
            &ToolName::plain("exec_command"),
            Some(&json!({"cmd": "long job"})),
            &json!({
                "session_id": 41,
                "chunk_id": "abc123",
                "output": "still running"
            }),
        );

        service.refresh_reduction_config(&disabled);
        service.refresh_reduction_config(&enabled);

        assert!(
            service
                .actionable_states
                .read(&cell_id, /*take*/ false)
                .is_some(),
            "live continuation handles must survive reducer reconfiguration"
        );
    }

    #[test]
    fn build_nested_tool_payload_uses_function_kind() {
        let payload = build_nested_tool_payload(
            CodeModeToolKind::Function,
            &ToolName::plain("example"),
            Some(json!({ "value": 1 })),
        )
        .expect("function payload should serialize");

        match payload {
            ToolPayload::Function { arguments } => {
                assert_eq!(arguments, r#"{"value":1}"#.to_string());
            }
            other => panic!("expected function payload, got {other:?}"),
        }
    }

    #[test]
    fn build_nested_tool_payload_uses_freeform_kind() {
        let payload = build_nested_tool_payload(
            CodeModeToolKind::Freeform,
            &ToolName::plain("example"),
            Some(json!("hello")),
        )
        .expect("freeform payload should preserve string input");

        match payload {
            ToolPayload::Custom { input } => {
                assert_eq!(input, "hello".to_string());
            }
            other => panic!("expected freeform payload, got {other:?}"),
        }
    }

    #[test]
    fn truncated_text_output_starts_with_warning() {
        let items = vec![FunctionCallOutputContentItem::InputText {
            text: "0123456789012345678901234567890123456789".to_string(),
        }];

        assert_eq!(
            truncate_code_mode_result(items, Some(5)),
            vec![FunctionCallOutputContentItem::InputText {
                text: concat!(
                    "Warning: truncated output (original token count: 10)\n",
                    "Total output lines: 1\n\n",
                    "0123456789…5 tokens truncated…0123456789"
                )
                .to_string(),
            }]
        );
    }

    #[test]
    fn over_budget_audio_output_is_omitted() {
        let items = vec![FunctionCallOutputContentItem::InputAudio {
            audio_url: format!("data:audio/wav;base64,{}", "A".repeat(100)),
        }];

        assert_eq!(
            truncate_code_mode_result(items, Some(5)),
            vec![FunctionCallOutputContentItem::InputText {
                text: "[omitted 1 audio items ...]".to_string(),
            }]
        );
    }
}
