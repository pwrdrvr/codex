use codex_protocol::models::ContentItemKind;

use super::ContextualUserFragment;

const MAX_RECEIPT_BYTES: usize = 1_024;
const MAX_RETRIEVAL_BYTES: usize = 8 * 1024;

/// Bounded data envelope supplied to the isolated reducer model.
pub(crate) struct TokenMiserReducerInput {
    body: String,
}

impl TokenMiserReducerInput {
    pub(crate) fn new(body: String, max_bytes: usize) -> Option<Self> {
        (body.len() <= max_bytes).then_some(Self { body })
    }
}

impl ContextualUserFragment for TokenMiserReducerInput {
    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind("token_miser.reducer_input".to_string())
    }

    fn role(&self) -> &'static str {
        "user"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("<token_miser_input>", "</token_miser_input>")
    }

    fn body(&self) -> String {
        self.body.clone()
    }
}

/// Trusted receipt for exact output retained outside model-visible history.
pub(crate) struct TokenMiserReceipt {
    body: String,
}

impl TokenMiserReceipt {
    pub(crate) fn retained(object_id: &str, decision: &str) -> Self {
        Self::new(format!(
            "Output {decision}. Exact output object: {object_id}. Use tools.read_token_miser_output with this object_id, item_index, offset, and max_bytes; use tools.search_token_miser_output to locate text."
        ))
    }

    pub(crate) fn storage_unavailable() -> Self {
        Self::new(
            "Tool output was withheld because durable Token Miser storage was unavailable. The turn may continue without the output."
                .to_string(),
        )
    }

    pub(crate) fn output_too_large() -> Self {
        Self::new(
            "Tool output exceeded Token Miser's durable storage bound and was withheld. The turn may continue without the output."
                .to_string(),
        )
    }

    fn new(body: String) -> Self {
        Self {
            body: truncate_utf8_bytes(body, MAX_RECEIPT_BYTES),
        }
    }
}

impl ContextualUserFragment for TokenMiserReceipt {
    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind("token_miser.receipt".to_string())
    }

    fn role(&self) -> &'static str {
        "user"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("<token_miser_receipt>", "</token_miser_receipt>")
    }

    fn body(&self) -> String {
        self.body.clone()
    }
}

/// Hard-bounded result returned by an explicit read or search operation.
pub(crate) struct TokenMiserRetrievalResult {
    body: String,
}

impl TokenMiserRetrievalResult {
    pub(crate) fn new(body: String) -> Self {
        Self {
            body: truncate_utf8_bytes(body, MAX_RETRIEVAL_BYTES),
        }
    }
}

impl ContextualUserFragment for TokenMiserRetrievalResult {
    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind("token_miser.retrieval".to_string())
    }

    fn role(&self) -> &'static str {
        "user"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("<token_miser_retrieval>", "</token_miser_retrieval>")
    }

    fn body(&self) -> String {
        self.body.clone()
    }
}

fn truncate_utf8_bytes(mut value: String, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    value
}
