use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::RwLock;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use codex_hooks::PostToolUseRequest;
use codex_http_client::HttpClient;
use codex_http_client::HttpClientBuilder;
use serde::Deserialize;
use serde::Serialize;
use tokio::io::AsyncReadExt;
use url::Host;
use url::Url;

pub(crate) const DESCRIPTOR_ENVIRONMENT_VARIABLE: &str =
    "PWRAGENT_TOKEN_MISER_BRIDGE_DESCRIPTOR_PATH";
pub(crate) const IDENTITY: &str = "pwrdrvr.pwragent.token-miser";
pub(crate) const PROTOCOL_VERSION: u32 = 1;

const MAX_DESCRIPTOR_BYTES: usize = 16 * 1024;
const MAX_REQUEST_BYTES: usize = 32 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_REPLACEMENT_CHARACTERS: usize = 4_000;
const MAX_RESPONSE_ID_CHARACTERS: usize = 256;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(55);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagedPostToolUseReplacement {
    pub(crate) text: String,
    pub(crate) response_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BridgeDescriptorWire {
    version: u32,
    identity: String,
    activation_nonce: String,
    url: String,
    acceptance_url: String,
    token: String,
}

struct BridgeDescriptor {
    url: Url,
    acceptance_url: Url,
    token: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BridgeResponse {
    #[serde(rename = "hookOutput")]
    hook_output: Option<ManagedHookOutput>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ManagedHookOutput {
    #[serde(rename = "continue")]
    continue_processing: bool,
    stop_reason: String,
    hook_specific_output: ManagedHookSpecificOutput,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ManagedHookSpecificOutput {
    hook_event_name: String,
    #[serde(rename = "response_id")]
    response_id: String,
}

#[derive(Debug, Serialize)]
struct PostToolUseAcceptance<'a> {
    version: u32,
    response_id: &'a str,
    session_id: &'a str,
    turn_id: &'a str,
    tool_use_id: &'a str,
}

pub(crate) struct PwrdrvrTokenMiserGate {
    activation_nonce: RwLock<Option<Arc<str>>>,
    client: Option<HttpClient>,
}

impl PwrdrvrTokenMiserGate {
    pub(crate) fn new() -> Self {
        let client = HttpClientBuilder::new()
            .connect_timeout(CONNECT_TIMEOUT)
            .without_request_logging()
            .without_redirects()
            .build_direct()
            .inspect_err(|error| {
                tracing::warn!(
                    %error,
                    "PwrAgent Token Miser disabled: HTTP client setup failed"
                );
            })
            .ok();
        Self {
            activation_nonce: RwLock::new(None),
            client,
        }
    }

    pub(crate) fn set_activation_nonce(&self, activation_nonce: Option<Arc<str>>) {
        *self
            .activation_nonce
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = activation_nonce;
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.activation_nonce
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
    }

    pub(crate) async fn run(
        &self,
        request: &PostToolUseRequest,
    ) -> Option<ManagedPostToolUseReplacement> {
        let client = self.client.as_ref()?;
        let activation_nonce = self
            .activation_nonce
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()?;
        let descriptor = read_descriptor(&activation_nonce).await.ok()?;
        let request_body = request.command_input_json().ok()?;
        if request_body.len() > MAX_REQUEST_BYTES {
            tracing::warn!(
                request_bytes = request_body.len(),
                limit = MAX_REQUEST_BYTES,
                "PwrAgent Token Miser request exceeded its byte limit"
            );
            return None;
        }
        let body = request_json(client, &descriptor, request_body).await?;
        let response = serde_json::from_slice::<BridgeResponse>(&body)
            .inspect_err(|error| {
                tracing::warn!(%error, "PwrAgent Token Miser returned a malformed response");
            })
            .ok()?;
        let hook_output = response.hook_output?;
        if hook_output.continue_processing
            || hook_output.hook_specific_output.hook_event_name != "PostToolUse"
            || !bounded_non_empty(&hook_output.stop_reason, MAX_REPLACEMENT_CHARACTERS)
            || !bounded_non_empty(
                &hook_output.hook_specific_output.response_id,
                MAX_RESPONSE_ID_CHARACTERS,
            )
        {
            tracing::warn!("PwrAgent Token Miser response violated the managed gate contract");
            return None;
        }
        Some(ManagedPostToolUseReplacement {
            text: hook_output.stop_reason,
            response_id: hook_output.hook_specific_output.response_id,
        })
    }

    pub(crate) async fn accept(
        &self,
        response_id: &str,
        session_id: &str,
        turn_id: &str,
        tool_use_id: &str,
    ) {
        let Some(client) = self.client.as_ref() else {
            return;
        };
        let activation_nonce = self
            .activation_nonce
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let Some(activation_nonce) = activation_nonce else {
            return;
        };
        let Ok(descriptor) = read_descriptor(&activation_nonce).await else {
            return;
        };
        let acceptance = PostToolUseAcceptance {
            version: PROTOCOL_VERSION,
            response_id,
            session_id,
            turn_id,
            tool_use_id,
        };
        let callback = client
            .post(descriptor.acceptance_url)
            .bearer_auth(&descriptor.token)
            .json(&acceptance)
            .send();
        match tokio::time::timeout(REQUEST_TIMEOUT, callback).await {
            Ok(Ok(response)) if response.status().is_success() => {}
            Ok(Ok(response)) => tracing::warn!(
                status = %response.status(),
                "PwrAgent Token Miser acceptance returned an error status"
            ),
            Ok(Err(error)) => tracing::warn!(
                %error,
                "PwrAgent Token Miser acceptance failed"
            ),
            Err(_) => tracing::warn!("PwrAgent Token Miser acceptance timed out"),
        }
    }
}

pub(crate) async fn validate_activation(activation_nonce: &str) -> Result<(), String> {
    validate_nonce(activation_nonce)?;
    read_descriptor(activation_nonce).await.map(|_| ())
}

async fn request_json(
    client: &HttpClient,
    descriptor: &BridgeDescriptor,
    request_body: String,
) -> Option<Vec<u8>> {
    tokio::time::timeout(REQUEST_TIMEOUT, async {
        let mut response = client
            .post(descriptor.url.clone())
            .bearer_auth(&descriptor.token)
            .header("content-type", "application/json")
            .body(request_body)
            .send()
            .await
            .inspect_err(|error| {
                tracing::warn!(%error, "PwrAgent Token Miser bridge was unreachable");
            })
            .ok()?;
        if !response.status().is_success()
            || response
                .content_length()
                .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return None;
        }
        let mut body = Vec::with_capacity(
            response
                .content_length()
                .and_then(|length| usize::try_from(length).ok())
                .unwrap_or_default()
                .min(MAX_RESPONSE_BYTES),
        );
        while let Some(chunk) = response.chunk().await.ok()? {
            if chunk.len() > MAX_RESPONSE_BYTES.saturating_sub(body.len()) {
                tracing::warn!("PwrAgent Token Miser response exceeded its byte limit");
                return None;
            }
            body.extend_from_slice(&chunk);
        }
        Some(body)
    })
    .await
    .unwrap_or_else(|_| {
        tracing::warn!("PwrAgent Token Miser request timed out");
        None
    })
}

async fn read_descriptor(expected_nonce: &str) -> Result<BridgeDescriptor, String> {
    let path = descriptor_path()?;
    validate_descriptor_file(&path).await?;
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|error| format!("failed to open Token Miser descriptor: {error}"))?;
    let mut contents = Vec::with_capacity(MAX_DESCRIPTOR_BYTES);
    file.take((MAX_DESCRIPTOR_BYTES + 1) as u64)
        .read_to_end(&mut contents)
        .await
        .map_err(|error| format!("failed to read Token Miser descriptor: {error}"))?;
    if contents.len() > MAX_DESCRIPTOR_BYTES {
        return Err("Token Miser descriptor exceeded its byte limit".to_string());
    }
    let descriptor = serde_json::from_slice::<BridgeDescriptorWire>(&contents)
        .map_err(|error| format!("Token Miser descriptor is malformed: {error}"))?;
    if descriptor.version != PROTOCOL_VERSION
        || descriptor.identity != IDENTITY
        || descriptor.activation_nonce != expected_nonce
    {
        return Err("Token Miser descriptor identity did not match initialize".to_string());
    }
    validate_nonce(&descriptor.activation_nonce)?;
    validate_token(&descriptor.token)?;
    Ok(BridgeDescriptor {
        url: parse_loopback_endpoint(&descriptor.url, "url")?,
        acceptance_url: parse_loopback_endpoint(&descriptor.acceptance_url, "acceptance_url")?,
        token: descriptor.token,
    })
}

fn descriptor_path() -> Result<PathBuf, String> {
    let path = std::env::var_os(DESCRIPTOR_ENVIRONMENT_VARIABLE)
        .map(PathBuf::from)
        .ok_or_else(|| format!("{DESCRIPTOR_ENVIRONMENT_VARIABLE} is not set"))?;
    if !path.is_absolute() {
        return Err("Token Miser descriptor path must be absolute".to_string());
    }
    Ok(path)
}

async fn validate_descriptor_file(path: &Path) -> Result<(), String> {
    let metadata = tokio::fs::symlink_metadata(path)
        .await
        .map_err(|error| format!("failed to inspect Token Miser descriptor: {error}"))?;
    if !metadata.file_type().is_file() {
        return Err("Token Miser descriptor must be a regular file".to_string());
    }
    if metadata.len() > MAX_DESCRIPTOR_BYTES as u64 {
        return Err("Token Miser descriptor exceeded its byte limit".to_string());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o077 != 0 {
            return Err("Token Miser descriptor permissions must be private".to_string());
        }
        let parent = path
            .parent()
            .ok_or_else(|| "Token Miser descriptor has no parent directory".to_string())?;
        let parent_metadata = tokio::fs::symlink_metadata(parent)
            .await
            .map_err(|error| format!("failed to inspect Token Miser directory: {error}"))?;
        if !parent_metadata.file_type().is_dir() || parent_metadata.mode() & 0o077 != 0 {
            return Err("Token Miser descriptor directory permissions must be private".to_string());
        }
    }
    Ok(())
}

fn parse_loopback_endpoint(endpoint: &str, field: &str) -> Result<Url, String> {
    let url = Url::parse(endpoint)
        .map_err(|error| format!("Token Miser {field} is malformed: {error}"))?;
    let is_loopback = match url.host() {
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        Some(Host::Domain(_)) | None => false,
    };
    if url.scheme() != "http"
        || !is_loopback
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(format!(
            "Token Miser {field} must be credential-free loopback HTTP"
        ));
    }
    Ok(url)
}

fn validate_nonce(nonce: &str) -> Result<(), String> {
    validate_base64_secret(nonce, "activation nonce")
}

fn validate_token(token: &str) -> Result<(), String> {
    validate_base64_secret(token, "bearer token")
}

fn validate_base64_secret(secret: &str, label: &str) -> Result<(), String> {
    let bytes = URL_SAFE_NO_PAD
        .decode(secret)
        .map_err(|_| format!("Token Miser {label} must be unpadded base64url"))?;
    if bytes.len() < 32 {
        return Err(format!(
            "Token Miser {label} must contain at least 256 bits"
        ));
    }
    Ok(())
}

fn bounded_non_empty(value: &str, max_characters: usize) -> bool {
    !value.trim().is_empty() && value.chars().take(max_characters + 1).count() <= max_characters
}
