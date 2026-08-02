//! User-configured AI providers and the bounded OpenAI-compatible transport
//! used by the uTools compatibility runtime.
//!
//! Provider metadata is ordinary host settings, while API keys live in the
//! existing encrypted storage namespace. Plugin iframes can select a model and
//! submit their own messages, but they never receive the configured endpoint
//! credential.

use std::{
    collections::{BTreeMap, HashSet},
    net::IpAddr,
    time::Duration,
};

use reqwest::{redirect::Policy, Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use url::{Host, Url};
use uuid::{Uuid, Version};
use zeroize::{Zeroize, Zeroizing};

use crate::{plugin_crypto_storage::PluginCryptoStorage, plugin_settings::PluginSettingsStore};

const SETTINGS_NAMESPACE: &str = "__ihub.ai.providers.v1";
const SETTINGS_KEY: &str = "profiles";
const SECRET_NAMESPACE: &str = "__ihub.ai.provider-secrets.v1";
const STATE_SCHEMA_VERSION: u32 = 1;
const MAX_PROVIDERS: usize = 16;
const MAX_MODELS_PER_PROVIDER: usize = 64;
const MAX_LABEL_CHARS: usize = 96;
const MAX_DESCRIPTION_CHARS: usize = 1_000;
const MAX_ENDPOINT_CHARS: usize = 2_048;
const MAX_MODEL_ID_CHARS: usize = 160;
const MAX_API_KEY_BYTES: usize = 8 * 1024;
const MAX_MESSAGES: usize = 128;
const MAX_MESSAGE_CHARS: usize = 256 * 1024;
const MAX_TOTAL_OPTION_BYTES: usize = 1024 * 1024;
const MAX_TOOLS: usize = 64;
const MAX_TOOL_SCHEMA_BYTES: usize = 64 * 1024;
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_ERROR_BYTES: usize = 64 * 1024;
const MAX_SSE_LINE_BYTES: usize = 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2 * 60);

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AiProviderModel {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AiProviderProfile {
    id: String,
    label: String,
    endpoint: String,
    models: Vec<AiProviderModel>,
    default_model: String,
    has_api_key: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PersistedAiProviderState {
    schema_version: u32,
    #[serde(default)]
    default_provider_id: Option<String>,
    #[serde(default)]
    profiles: Vec<AiProviderProfile>,
}

impl Default for PersistedAiProviderState {
    fn default() -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            default_provider_id: None,
            profiles: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiProviderProfileView {
    pub id: String,
    pub label: String,
    pub endpoint: String,
    pub models: Vec<AiProviderModel>,
    pub default_model: String,
    pub has_api_key: bool,
    pub is_default: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveAiProviderProfileInput {
    #[serde(default)]
    pub id: Option<String>,
    pub label: String,
    pub endpoint: String,
    pub models: Vec<AiProviderModel>,
    pub default_model: String,
    /// `None` preserves an existing secret. An empty string explicitly clears
    /// it; any other value replaces it.
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub make_default: bool,
}

#[derive(Clone)]
pub struct AiProviderStore {
    settings: PluginSettingsStore,
    secrets: PluginCryptoStorage,
}

#[derive(Debug)]
pub struct ResolvedAiModel {
    pub endpoint: Url,
    pub remote_model: String,
    pub api_key: Option<Zeroizing<String>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UtoolsAiOption {
    #[serde(default)]
    pub model: Option<String>,
    pub messages: Vec<UtoolsAiMessage>,
    #[serde(default)]
    pub tools: Vec<UtoolsAiTool>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UtoolsAiMessage {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UtoolsAiTool {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: UtoolsAiFunction,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UtoolsAiFunction {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AiToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
    arguments_json: String,
}

#[derive(Clone, Debug)]
pub struct AiChatRound {
    pub message: UtoolsAiMessage,
    pub tool_calls: Vec<AiToolCall>,
    pub assistant_wire_message: Value,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UtoolsAiModelView {
    pub id: String,
    pub label: String,
    pub description: String,
    pub icon: String,
    pub cost: u32,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiProviderTestResult {
    pub reachable: bool,
    pub model_ids: Vec<String>,
    pub message: String,
}

impl AiProviderStore {
    pub fn new(settings: PluginSettingsStore, secrets: PluginCryptoStorage) -> Self {
        Self { settings, secrets }
    }

    pub fn list_profiles(&self) -> Result<Vec<AiProviderProfileView>, String> {
        let state = self.load_state()?;
        Ok(state
            .profiles
            .into_iter()
            .map(|profile| AiProviderProfileView {
                is_default: state.default_provider_id.as_deref() == Some(profile.id.as_str()),
                id: profile.id,
                label: profile.label,
                endpoint: profile.endpoint,
                models: profile.models,
                default_model: profile.default_model,
                has_api_key: profile.has_api_key,
            })
            .collect())
    }

    pub fn save_profile(
        &self,
        input: SaveAiProviderProfileInput,
    ) -> Result<AiProviderProfileView, String> {
        let mut state = self.load_state()?;
        let id = match input.id.as_deref() {
            Some(id) => validate_profile_id(id)?,
            None => Uuid::new_v4().to_string(),
        };
        let label = validate_text("Provider label", &input.label, MAX_LABEL_CHARS)?;
        let endpoint = normalize_endpoint(&input.endpoint)?.to_string();
        let models = validate_models(input.models)?;
        let default_model = input.default_model.trim().to_owned();
        if !models.iter().any(|model| model.id == default_model) {
            return Err("The default model must be one of this provider's model IDs.".to_owned());
        }
        let existing_index = state.profiles.iter().position(|profile| profile.id == id);
        if existing_index.is_none() && state.profiles.len() >= MAX_PROVIDERS {
            return Err(format!(
                "iHub supports at most {MAX_PROVIDERS} AI providers."
            ));
        }
        let old_secret = self.secret_for(&id)?;
        let requested_secret = input.api_key.map(|mut value| {
            let trimmed = Zeroizing::new(value.trim().to_owned());
            value.zeroize();
            trimmed
        });
        if let Some(secret) = requested_secret.as_deref() {
            validate_api_key(secret)?;
            self.write_secret(&id, secret)?;
        }
        let has_api_key = requested_secret
            .as_ref()
            .map(|secret| !secret.is_empty())
            .unwrap_or_else(|| {
                existing_index
                    .and_then(|index| state.profiles.get(index))
                    .is_some_and(|profile| profile.has_api_key)
            });
        let profile = AiProviderProfile {
            id: id.clone(),
            label,
            endpoint,
            models,
            default_model,
            has_api_key,
        };
        if let Some(index) = existing_index {
            state.profiles[index] = profile.clone();
        } else {
            state.profiles.push(profile.clone());
        }
        if input.make_default || state.default_provider_id.is_none() {
            state.default_provider_id = Some(id.clone());
        }
        if let Err(error) = self.persist_state(&state) {
            let _ = self.restore_secret(&id, old_secret.as_deref().map(String::as_str));
            return Err(error);
        }
        Ok(AiProviderProfileView {
            is_default: state.default_provider_id.as_deref() == Some(id.as_str()),
            id: profile.id,
            label: profile.label,
            endpoint: profile.endpoint,
            models: profile.models,
            default_model: profile.default_model,
            has_api_key: profile.has_api_key,
        })
    }

    pub fn delete_profile(&self, profile_id: &str) -> Result<bool, String> {
        let id = validate_profile_id(profile_id)?;
        let mut state = self.load_state()?;
        let Some(index) = state.profiles.iter().position(|profile| profile.id == id) else {
            return Ok(false);
        };
        let old_secret = self.secret_for(&id)?;
        self.write_secret(&id, "")?;
        state.profiles.remove(index);
        if state.default_provider_id.as_deref() == Some(id.as_str()) {
            state.default_provider_id = state.profiles.first().map(|profile| profile.id.clone());
        }
        if let Err(error) = self.persist_state(&state) {
            let _ = self.restore_secret(&id, old_secret.as_deref().map(String::as_str));
            return Err(error);
        }
        Ok(true)
    }

    pub fn list_models(&self) -> Result<Vec<UtoolsAiModelView>, String> {
        let state = self.load_state()?;
        let mut models = state
            .profiles
            .iter()
            .flat_map(|profile| {
                profile.models.iter().map(|model| UtoolsAiModelView {
                    id: canonical_model_id(&profile.id, &model.id),
                    label: format!("{} · {}", profile.label, model.label),
                    description: model.description.clone(),
                    icon: String::new(),
                    cost: 0,
                })
            })
            .collect::<Vec<_>>();
        if let Some(default_provider) = state.default_provider_id.as_deref() {
            models.sort_by_key(|model| !model.id.starts_with(&format!("{default_provider}::")));
        }
        Ok(models)
    }

    pub fn resolve_model(&self, requested: Option<&str>) -> Result<ResolvedAiModel, String> {
        let state = self.load_state()?;
        if state.profiles.is_empty() {
            return Err("No AI provider is configured in iHub preferences.".to_owned());
        }
        let (profile, model) = match requested.map(str::trim).filter(|value| !value.is_empty()) {
            None => {
                let profile_id = state
                    .default_provider_id
                    .as_deref()
                    .ok_or_else(|| "No default AI provider is configured.".to_owned())?;
                let profile = state
                    .profiles
                    .iter()
                    .find(|profile| profile.id == profile_id)
                    .ok_or_else(|| "The default AI provider no longer exists.".to_owned())?;
                let model = profile
                    .models
                    .iter()
                    .find(|model| model.id == profile.default_model)
                    .ok_or_else(|| "The default AI model no longer exists.".to_owned())?;
                (profile, model)
            }
            Some(requested) => {
                if let Some((provider_id, remote_id)) = requested.split_once("::") {
                    let profile = state
                        .profiles
                        .iter()
                        .find(|profile| profile.id == provider_id)
                        .ok_or_else(|| format!("Unknown AI provider model '{requested}'."))?;
                    let model = profile
                        .models
                        .iter()
                        .find(|model| model.id == remote_id)
                        .ok_or_else(|| format!("Unknown AI provider model '{requested}'."))?;
                    (profile, model)
                } else {
                    let matches = state
                        .profiles
                        .iter()
                        .flat_map(|profile| {
                            profile
                                .models
                                .iter()
                                .filter(move |model| model.id == requested)
                                .map(move |model| (profile, model))
                        })
                        .collect::<Vec<_>>();
                    match matches.as_slice() {
                        [single] => *single,
                        [] => return Err(format!("Unknown AI model '{requested}'.")),
                        _ => {
                            return Err(format!(
                                "AI model '{requested}' exists in multiple providers; use the ID returned by allAiModels()."
                            ))
                        }
                    }
                }
            }
        };
        let api_key = if profile.has_api_key {
            Some(
                self.secret_for(&profile.id)?
                    .filter(|secret| !secret.is_empty())
                    .ok_or_else(|| {
                        format!(
                            "AI provider '{}' is missing its encrypted API key.",
                            profile.label
                        )
                    })?,
            )
        } else {
            None
        };
        Ok(ResolvedAiModel {
            endpoint: normalize_endpoint(&profile.endpoint)?,
            remote_model: model.id.clone(),
            api_key,
        })
    }

    pub async fn test_profile(&self, profile_id: &str) -> Result<AiProviderTestResult, String> {
        let id = validate_profile_id(profile_id)?;
        let state = self.load_state()?;
        let profile = state
            .profiles
            .iter()
            .find(|profile| profile.id == id)
            .ok_or_else(|| "The AI provider no longer exists.".to_owned())?;
        let canonical = canonical_model_id(&profile.id, &profile.default_model);
        let resolved = self.resolve_model(Some(&canonical))?;
        let endpoint = resolved
            .endpoint
            .join("models")
            .map_err(|error| format!("Could not build AI models endpoint: {error}"))?;
        let client = ai_http_client()?;
        let mut request = client.get(endpoint);
        if let Some(api_key) = resolved.api_key.as_deref() {
            request = request.bearer_auth(api_key);
        }
        let response = request
            .send()
            .await
            .map_err(|error| request_error_message(&error))?;
        let status = response.status();
        if !status.is_success() {
            return Err(read_ai_error(response, status).await);
        }
        let value = read_limited_json(response, 1024 * 1024).await?;
        let model_ids = value
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| "AI provider /models response has no data array.".to_owned())?
            .iter()
            .filter_map(|model| model.get("id").and_then(Value::as_str))
            .take(128)
            .filter_map(|id| validate_model_id(id).ok())
            .collect::<Vec<_>>();
        Ok(AiProviderTestResult {
            reachable: true,
            message: if model_ids.is_empty() {
                "Provider 可连接，但 /models 未返回可用模型 ID。".to_owned()
            } else {
                format!("Provider 可连接，共返回 {} 个模型。", model_ids.len())
            },
            model_ids,
        })
    }

    fn load_state(&self) -> Result<PersistedAiProviderState, String> {
        let Some(value) = self.settings.get(SETTINGS_NAMESPACE, SETTINGS_KEY) else {
            return Ok(PersistedAiProviderState::default());
        };
        let state = serde_json::from_value::<PersistedAiProviderState>(value)
            .map_err(|error| format!("Could not decode AI provider settings: {error}"))?;
        validate_state(state)
    }

    fn persist_state(&self, state: &PersistedAiProviderState) -> Result<(), String> {
        let value = serde_json::to_value(state)
            .map_err(|error| format!("Could not encode AI provider settings: {error}"))?;
        self.settings.set(SETTINGS_NAMESPACE, SETTINGS_KEY, value)
    }

    fn secret_for(&self, profile_id: &str) -> Result<Option<Zeroizing<String>>, String> {
        let mut snapshot = self.secrets.snapshot(SECRET_NAMESPACE)?;
        let secret = match snapshot.remove(profile_id) {
            Some(Value::String(secret)) => Some(Zeroizing::new(secret)),
            _ => None,
        };
        for value in snapshot.values_mut() {
            if let Value::String(secret) = value {
                secret.zeroize();
            }
        }
        Ok(secret)
    }

    fn write_secret(&self, profile_id: &str, secret: &str) -> Result<(), String> {
        if secret.is_empty() {
            self.secrets.remove(SECRET_NAMESPACE, profile_id)?;
            Ok(())
        } else {
            self.secrets.set(
                SECRET_NAMESPACE,
                profile_id,
                Value::String(secret.to_owned()),
            )
        }
    }

    fn restore_secret(&self, profile_id: &str, secret: Option<&str>) -> Result<(), String> {
        self.write_secret(profile_id, secret.unwrap_or_default())
    }
}

pub fn validate_ai_option(option: UtoolsAiOption) -> Result<UtoolsAiOption, String> {
    if option.messages.is_empty() || option.messages.len() > MAX_MESSAGES {
        return Err(format!("utools.ai requires 1-{MAX_MESSAGES} messages."));
    }
    for message in &option.messages {
        if !matches!(message.role.as_str(), "system" | "user" | "assistant") {
            return Err(
                "utools.ai accepts only system, user, and assistant message roles.".to_owned(),
            );
        }
        validate_optional_message_text("message content", message.content.as_deref())?;
        validate_optional_message_text(
            "message reasoning_content",
            message.reasoning_content.as_deref(),
        )?;
        if message.content.is_none() && message.reasoning_content.is_none() {
            return Err("Each utools.ai message requires content or reasoning_content.".to_owned());
        }
    }
    if option.tools.len() > MAX_TOOLS {
        return Err(format!(
            "utools.ai accepts at most {MAX_TOOLS} function tools."
        ));
    }
    let mut names = HashSet::new();
    for tool in &option.tools {
        if tool.kind != "function" {
            return Err("utools.ai supports only function tools.".to_owned());
        }
        validate_function_name(&tool.function.name)?;
        if !names.insert(tool.function.name.as_str()) {
            return Err(format!("Duplicate AI function '{}'.", tool.function.name));
        }
        validate_text(
            "AI function description",
            &tool.function.description,
            MAX_DESCRIPTION_CHARS,
        )?;
        if !tool.function.parameters.is_object() {
            return Err(format!(
                "AI function '{}' parameters must be a JSON Schema object.",
                tool.function.name
            ));
        }
        let schema_bytes = serde_json::to_vec(&tool.function.parameters)
            .map_err(|error| format!("Could not encode AI function schema: {error}"))?;
        if schema_bytes.len() > MAX_TOOL_SCHEMA_BYTES {
            return Err(format!(
                "AI function '{}' schema exceeds {MAX_TOOL_SCHEMA_BYTES} bytes.",
                tool.function.name
            ));
        }
        for required in &tool.function.required {
            if required.is_empty() || required.chars().count() > MAX_MODEL_ID_CHARS {
                return Err("AI function required field name is invalid.".to_owned());
            }
        }
    }
    let encoded = serde_json::to_vec(&option)
        .map_err(|error| format!("Could not encode utools.ai options: {error}"))?;
    if encoded.len() > MAX_TOTAL_OPTION_BYTES {
        return Err(format!(
            "utools.ai options exceed {MAX_TOTAL_OPTION_BYTES} bytes."
        ));
    }
    Ok(option)
}

pub async fn execute_chat_round<F>(
    resolved: &ResolvedAiModel,
    wire_messages: &[Value],
    tools: &[UtoolsAiTool],
    stream: bool,
    mut on_chunk: F,
) -> Result<AiChatRound, String>
where
    F: FnMut(UtoolsAiMessage) -> Result<(), String>,
{
    let client = ai_http_client()?;
    let endpoint = resolved
        .endpoint
        .join("chat/completions")
        .map_err(|error| format!("Could not build AI chat endpoint: {error}"))?;
    let mut body = Map::new();
    body.insert(
        "model".to_owned(),
        Value::String(resolved.remote_model.clone()),
    );
    body.insert("messages".to_owned(), Value::Array(wire_messages.to_vec()));
    body.insert("stream".to_owned(), Value::Bool(stream));
    if !tools.is_empty() {
        body.insert(
            "tools".to_owned(),
            serde_json::to_value(to_wire_tools(tools))
                .map_err(|error| format!("Could not encode AI tools: {error}"))?,
        );
    }
    let mut request = client.post(endpoint).json(&body);
    if let Some(api_key) = resolved.api_key.as_deref() {
        request = request.bearer_auth(api_key);
    }
    let response = request
        .send()
        .await
        .map_err(|error| request_error_message(&error))?;
    let status = response.status();
    if !status.is_success() {
        return Err(read_ai_error(response, status).await);
    }
    let is_event_stream = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().starts_with("text/event-stream"));
    if stream && is_event_stream {
        parse_streaming_response(response, &mut on_chunk).await
    } else {
        let value = read_limited_json(response, MAX_RESPONSE_BYTES).await?;
        parse_non_streaming_response(value, stream.then_some(&mut on_chunk))
    }
}

pub fn initial_wire_messages(option: &UtoolsAiOption) -> Vec<Value> {
    option
        .messages
        .iter()
        .map(|message| {
            let mut value = json!({ "role": message.role });
            if let Some(content) = message.content.as_deref() {
                value["content"] = Value::String(content.to_owned());
            }
            if let Some(reasoning) = message.reasoning_content.as_deref() {
                value["reasoning_content"] = Value::String(reasoning.to_owned());
            }
            value
        })
        .collect()
}

pub fn tool_result_wire_message(call: &AiToolCall, result: &Value) -> Result<Value, String> {
    let content = serde_json::to_string(result)
        .map_err(|error| format!("Could not encode AI function result: {error}"))?;
    if content.len() > MAX_MESSAGE_CHARS {
        return Err("AI function result is too large.".to_owned());
    }
    Ok(json!({
        "role": "tool",
        "tool_call_id": call.id,
        "content": content,
    }))
}

fn to_wire_tools(tools: &[UtoolsAiTool]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            let mut parameters = tool.function.parameters.clone();
            if !tool.function.required.is_empty() {
                parameters["required"] = json!(tool.function.required);
            }
            json!({
                "type": "function",
                "function": {
                    "name": tool.function.name,
                    "description": tool.function.description,
                    "parameters": parameters,
                }
            })
        })
        .collect()
}

async fn parse_streaming_response<F>(
    mut response: reqwest::Response,
    on_chunk: &mut F,
) -> Result<AiChatRound, String>
where
    F: FnMut(UtoolsAiMessage) -> Result<(), String>,
{
    let mut buffer = Vec::<u8>::new();
    let mut received = 0usize;
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut tool_builders = BTreeMap::<usize, ToolCallBuilder>::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| request_error_message(&error))?
    {
        received = received.saturating_add(chunk.len());
        if received > MAX_RESPONSE_BYTES {
            return Err(format!("AI response exceeds {MAX_RESPONSE_BYTES} bytes."));
        }
        buffer.extend_from_slice(&chunk);
        while let Some(newline) = buffer.iter().position(|byte| *byte == b'\n') {
            let mut line = buffer.drain(..=newline).collect::<Vec<_>>();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.len() > MAX_SSE_LINE_BYTES {
                return Err("AI stream event exceeds the line limit.".to_owned());
            }
            process_sse_line(
                &line,
                on_chunk,
                &mut content,
                &mut reasoning,
                &mut tool_builders,
            )?;
        }
        if buffer.len() > MAX_SSE_LINE_BYTES {
            return Err("AI stream event exceeds the line limit.".to_owned());
        }
    }
    if !buffer.is_empty() {
        process_sse_line(
            &buffer,
            on_chunk,
            &mut content,
            &mut reasoning,
            &mut tool_builders,
        )?;
    }
    finish_round(content, reasoning, tool_builders)
}

fn process_sse_line<F>(
    line: &[u8],
    on_chunk: &mut F,
    content: &mut String,
    reasoning: &mut String,
    tool_builders: &mut BTreeMap<usize, ToolCallBuilder>,
) -> Result<(), String>
where
    F: FnMut(UtoolsAiMessage) -> Result<(), String>,
{
    if line.is_empty() || line.starts_with(b":") || !line.starts_with(b"data:") {
        return Ok(());
    }
    let payload = std::str::from_utf8(&line[5..])
        .map_err(|_| "AI stream contains invalid UTF-8.".to_owned())?
        .trim();
    if payload.is_empty() || payload == "[DONE]" {
        return Ok(());
    }
    let value = serde_json::from_str::<Value>(payload)
        .map_err(|error| format!("Could not decode AI stream event: {error}"))?;
    if let Some(message) = provider_error(&value) {
        return Err(message);
    }
    let delta = value
        .pointer("/choices/0/delta")
        .and_then(Value::as_object)
        .ok_or_else(|| "AI stream event has no first choice delta.".to_owned())?;
    let content_delta = delta.get("content").and_then(Value::as_str);
    let reasoning_delta = delta
        .get("reasoning_content")
        .or_else(|| delta.get("reasoning"))
        .and_then(Value::as_str);
    if let Some(part) = content_delta {
        append_bounded(content, part, "AI content")?;
    }
    if let Some(part) = reasoning_delta {
        append_bounded(reasoning, part, "AI reasoning")?;
    }
    if content_delta.is_some() || reasoning_delta.is_some() {
        on_chunk(UtoolsAiMessage {
            role: "assistant".to_owned(),
            content: content_delta.map(str::to_owned),
            reasoning_content: reasoning_delta.map(str::to_owned),
        })?;
    }
    if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
        for call in tool_calls {
            let index = call
                .get("index")
                .and_then(Value::as_u64)
                .and_then(|index| usize::try_from(index).ok())
                .ok_or_else(|| "AI stream tool call has no bounded index.".to_owned())?;
            if index >= MAX_TOOLS {
                return Err("AI stream returned too many tool calls.".to_owned());
            }
            let builder = tool_builders.entry(index).or_default();
            if let Some(id) = call.get("id").and_then(Value::as_str) {
                builder.id = id.to_owned();
            }
            if let Some(name) = call.pointer("/function/name").and_then(Value::as_str) {
                append_bounded(&mut builder.name, name, "AI tool name")?;
            }
            if let Some(arguments) = call.pointer("/function/arguments").and_then(Value::as_str) {
                append_bounded(&mut builder.arguments, arguments, "AI tool arguments")?;
            }
        }
    }
    Ok(())
}

fn parse_non_streaming_response<F>(
    value: Value,
    mut on_chunk: Option<&mut F>,
) -> Result<AiChatRound, String>
where
    F: FnMut(UtoolsAiMessage) -> Result<(), String>,
{
    if let Some(message) = provider_error(&value) {
        return Err(message);
    }
    let message = value
        .pointer("/choices/0/message")
        .and_then(Value::as_object)
        .ok_or_else(|| "AI response has no first choice message.".to_owned())?;
    let content = message
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let reasoning = message
        .get("reasoning_content")
        .or_else(|| message.get("reasoning"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    validate_optional_message_text(
        "AI content",
        (!content.is_empty()).then_some(content.as_str()),
    )?;
    validate_optional_message_text(
        "AI reasoning",
        (!reasoning.is_empty()).then_some(reasoning.as_str()),
    )?;
    if let Some(callback) = on_chunk.as_mut() {
        if !content.is_empty() || !reasoning.is_empty() {
            callback(UtoolsAiMessage {
                role: "assistant".to_owned(),
                content: (!content.is_empty()).then_some(content.clone()),
                reasoning_content: (!reasoning.is_empty()).then_some(reasoning.clone()),
            })?;
        }
    }
    let mut builders = BTreeMap::new();
    if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
        for (index, call) in tool_calls.iter().enumerate() {
            builders.insert(
                index,
                ToolCallBuilder {
                    id: call
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    name: call
                        .pointer("/function/name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    arguments: call
                        .pointer("/function/arguments")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                },
            );
        }
    }
    finish_round(content, reasoning, builders)
}

#[derive(Default)]
struct ToolCallBuilder {
    id: String,
    name: String,
    arguments: String,
}

fn finish_round(
    content: String,
    reasoning: String,
    builders: BTreeMap<usize, ToolCallBuilder>,
) -> Result<AiChatRound, String> {
    let mut tool_calls = Vec::with_capacity(builders.len());
    for (_, builder) in builders {
        if builder.id.is_empty() || builder.id.chars().count() > 256 {
            return Err("AI response contains an invalid tool call ID.".to_owned());
        }
        validate_function_name(&builder.name)?;
        if builder.arguments.len() > MAX_MESSAGE_CHARS {
            return Err("AI tool arguments are too large.".to_owned());
        }
        let arguments = serde_json::from_str::<Value>(&builder.arguments).map_err(|error| {
            format!(
                "AI tool '{}' returned invalid JSON arguments: {error}",
                builder.name
            )
        })?;
        if !arguments.is_object() {
            return Err(format!(
                "AI tool '{}' arguments must be an object.",
                builder.name
            ));
        }
        tool_calls.push(AiToolCall {
            id: builder.id,
            name: builder.name,
            arguments,
            arguments_json: builder.arguments,
        });
    }
    let message = UtoolsAiMessage {
        role: "assistant".to_owned(),
        content: (!content.is_empty()).then_some(content.clone()),
        reasoning_content: (!reasoning.is_empty()).then_some(reasoning.clone()),
    };
    let mut assistant_wire_message = json!({ "role": "assistant" });
    if !content.is_empty() {
        assistant_wire_message["content"] = Value::String(content);
    } else if !tool_calls.is_empty() {
        assistant_wire_message["content"] = Value::Null;
    }
    if !reasoning.is_empty() {
        assistant_wire_message["reasoning_content"] = Value::String(reasoning);
    }
    if !tool_calls.is_empty() {
        assistant_wire_message["tool_calls"] = Value::Array(
            tool_calls
                .iter()
                .map(|call| {
                    json!({
                        "id": call.id,
                        "type": "function",
                        "function": {
                            "name": call.name,
                            "arguments": call.arguments_json,
                        }
                    })
                })
                .collect(),
        );
    }
    Ok(AiChatRound {
        message,
        tool_calls,
        assistant_wire_message,
    })
}

fn validate_state(mut state: PersistedAiProviderState) -> Result<PersistedAiProviderState, String> {
    if state.schema_version != STATE_SCHEMA_VERSION || state.profiles.len() > MAX_PROVIDERS {
        return Err("AI provider settings have an unsupported or oversized schema.".to_owned());
    }
    let mut ids = HashSet::new();
    for profile in &mut state.profiles {
        profile.id = validate_profile_id(&profile.id)?;
        profile.label = validate_text("Provider label", &profile.label, MAX_LABEL_CHARS)?;
        profile.endpoint = normalize_endpoint(&profile.endpoint)?.to_string();
        profile.models = validate_models(std::mem::take(&mut profile.models))?;
        if !profile
            .models
            .iter()
            .any(|model| model.id == profile.default_model)
        {
            return Err(format!(
                "AI provider '{}' has an invalid default model.",
                profile.label
            ));
        }
        if !ids.insert(profile.id.clone()) {
            return Err("AI provider settings contain duplicate profile IDs.".to_owned());
        }
    }
    if state
        .default_provider_id
        .as_deref()
        .is_some_and(|id| !ids.contains(id))
    {
        return Err("The default AI provider does not exist.".to_owned());
    }
    Ok(state)
}

fn validate_models(models: Vec<AiProviderModel>) -> Result<Vec<AiProviderModel>, String> {
    if models.is_empty() || models.len() > MAX_MODELS_PER_PROVIDER {
        return Err(format!(
            "Each AI provider requires 1-{MAX_MODELS_PER_PROVIDER} models."
        ));
    }
    let mut ids = HashSet::new();
    models
        .into_iter()
        .map(|model| {
            let id = validate_model_id(&model.id)?;
            if !ids.insert(id.clone()) {
                return Err(format!("Duplicate AI model ID '{id}'."));
            }
            Ok(AiProviderModel {
                label: validate_text("Model label", &model.label, MAX_LABEL_CHARS)?,
                description: validate_optional_text(
                    "Model description",
                    &model.description,
                    MAX_DESCRIPTION_CHARS,
                )?,
                id,
            })
        })
        .collect()
}

fn normalize_endpoint(value: &str) -> Result<Url, String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > MAX_ENDPOINT_CHARS {
        return Err("AI provider endpoint is empty or too long.".to_owned());
    }
    let mut url = Url::parse(value)
        .map_err(|error| format!("AI provider endpoint is not an absolute URL: {error}"))?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(
            "AI provider endpoint cannot contain credentials, query, or fragment.".to_owned(),
        );
    }
    let local_http = url.scheme() == "http" && is_loopback_host(url.host());
    if url.scheme() != "https" && !local_http {
        return Err(
            "AI provider endpoint must use HTTPS; HTTP is allowed only for localhost or loopback IPs."
                .to_owned(),
        );
    }
    if url.cannot_be_a_base() || url.host().is_none() {
        return Err("AI provider endpoint must be a hierarchical URL with a host.".to_owned());
    }
    let mut path = url.path().trim_end_matches('/').to_owned();
    if path.is_empty() {
        path = "/v1".to_owned();
    }
    if !path.ends_with("/v1") {
        return Err("AI provider endpoint must end with /v1.".to_owned());
    }
    path.push('/');
    url.set_path(&path);
    Ok(url)
}

fn is_loopback_host(host: Option<Host<&str>>) -> bool {
    match host {
        Some(Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(address)) => IpAddr::V4(address).is_loopback(),
        Some(Host::Ipv6(address)) => IpAddr::V6(address).is_loopback(),
        None => false,
    }
}

fn validate_profile_id(value: &str) -> Result<String, String> {
    let parsed = Uuid::parse_str(value).map_err(|_| "AI provider ID must be a UUID.".to_owned())?;
    if parsed.get_version() != Some(Version::Random) {
        return Err("AI provider ID must be a version 4 UUID.".to_owned());
    }
    Ok(parsed.hyphenated().to_string())
}

fn validate_model_id(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty()
        || value.chars().count() > MAX_MODEL_ID_CHARS
        || value.chars().any(char::is_control)
    {
        return Err("AI model ID is empty, too long, or contains control characters.".to_owned());
    }
    Ok(value.to_owned())
}

fn validate_function_name(value: &str) -> Result<(), String> {
    let mut characters = value.chars();
    let valid_first = characters
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic());
    if !valid_first
        || value.chars().count() > 64
        || characters.any(|character| character != '_' && !character.is_ascii_alphanumeric())
    {
        return Err(format!("Invalid AI function name '{value}'."));
    }
    Ok(())
}

fn validate_api_key(secret: &str) -> Result<(), String> {
    if secret.len() > MAX_API_KEY_BYTES || secret.chars().any(char::is_control) {
        return Err(format!(
            "AI API key must contain at most {MAX_API_KEY_BYTES} bytes and no control characters."
        ));
    }
    Ok(())
}

fn validate_text(label: &str, value: &str, maximum: usize) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > maximum || value.chars().any(char::is_control) {
        return Err(format!(
            "{label} must contain 1-{maximum} visible characters."
        ));
    }
    Ok(value.to_owned())
}

fn validate_optional_text(label: &str, value: &str, maximum: usize) -> Result<String, String> {
    let value = value.trim();
    if value.chars().count() > maximum || value.chars().any(char::is_control) {
        return Err(format!(
            "{label} exceeds {maximum} characters or contains controls."
        ));
    }
    Ok(value.to_owned())
}

fn validate_optional_message_text(label: &str, value: Option<&str>) -> Result<(), String> {
    if value.is_some_and(|value| value.chars().count() > MAX_MESSAGE_CHARS || value.contains('\0'))
    {
        return Err(format!("{label} is too long or contains a NUL character."));
    }
    Ok(())
}

fn canonical_model_id(provider_id: &str, remote_id: &str) -> String {
    format!("{provider_id}::{remote_id}")
}

fn append_bounded(target: &mut String, part: &str, label: &str) -> Result<(), String> {
    if target.len().saturating_add(part.len()) > MAX_MESSAGE_CHARS {
        return Err(format!("{label} exceeds the response limit."));
    }
    target.push_str(part);
    Ok(())
}

fn ai_http_client() -> Result<Client, String> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    Client::builder()
        .redirect(Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .user_agent(concat!("iHub/", env!("CARGO_PKG_VERSION"), " AI Provider"))
        .build()
        .map_err(|error| format!("Could not initialize AI HTTP client: {error}"))
}

async fn read_limited_json(response: reqwest::Response, maximum: usize) -> Result<Value, String> {
    let bytes = read_limited_bytes(response, maximum).await?;
    serde_json::from_slice(&bytes).map_err(|error| format!("Could not decode AI response: {error}"))
}

async fn read_limited_bytes(
    mut response: reqwest::Response,
    maximum: usize,
) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| request_error_message(&error))?
    {
        if bytes.len().saturating_add(chunk.len()) > maximum {
            return Err(format!("AI response exceeds {maximum} bytes."));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn read_ai_error(response: reqwest::Response, status: StatusCode) -> String {
    let body = read_limited_bytes(response, MAX_ERROR_BYTES)
        .await
        .unwrap_or_default();
    let detail = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|value| provider_error(&value))
        .or_else(|| {
            std::str::from_utf8(&body)
                .ok()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| value.chars().take(1_000).collect())
        })
        .unwrap_or_else(|| "The provider returned no error detail.".to_owned());
    format!("AI provider returned HTTP {}: {detail}", status.as_u16())
}

fn provider_error(value: &Value) -> Option<String> {
    value
        .pointer("/error/message")
        .or_else(|| value.get("error").filter(|value| value.is_string()))
        .and_then(Value::as_str)
        .map(|message| message.chars().take(1_000).collect())
}

fn request_error_message(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "AI provider request timed out.".to_owned()
    } else if error.is_connect() {
        "Could not connect to the configured AI provider.".to_owned()
    } else {
        "AI provider request failed before a valid response was received.".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        finish_round, normalize_endpoint, parse_non_streaming_response, process_sse_line,
        validate_ai_option, validate_models, AiProviderModel, ToolCallBuilder, UtoolsAiFunction,
        UtoolsAiMessage, UtoolsAiOption, UtoolsAiTool,
    };
    use serde_json::json;
    use std::collections::BTreeMap;

    #[test]
    fn endpoints_require_https_or_exact_loopback_http_and_v1() {
        assert_eq!(
            normalize_endpoint("https://api.example.test/v1")
                .expect("https endpoint")
                .as_str(),
            "https://api.example.test/v1/"
        );
        assert!(normalize_endpoint("http://127.0.0.1:11434/v1").is_ok());
        assert!(normalize_endpoint("http://localhost:1234/v1/").is_ok());
        assert!(normalize_endpoint("http://192.168.1.10/v1").is_err());
        assert!(normalize_endpoint("https://api.example.test/openai").is_err());
        assert!(normalize_endpoint("https://user:secret@example.test/v1").is_err());
        assert!(normalize_endpoint("file:///v1").is_err());
    }

    #[test]
    fn provider_models_are_nonempty_unique_and_allow_ollama_tags() {
        assert!(validate_models(vec![AiProviderModel {
            id: "local/model".to_owned(),
            label: "Local model".to_owned(),
            description: String::new(),
        }])
        .is_ok());
        assert!(validate_models(vec![AiProviderModel {
            id: "qwen3:8b".to_owned(),
            label: "Ollama model".to_owned(),
            description: String::new(),
        }])
        .is_ok());
    }

    #[test]
    fn public_utools_ai_options_are_bounded_and_function_only() {
        let option = UtoolsAiOption {
            model: None,
            messages: vec![UtoolsAiMessage {
                role: "user".to_owned(),
                content: Some("hello".to_owned()),
                reasoning_content: None,
            }],
            tools: vec![UtoolsAiTool {
                kind: "function".to_owned(),
                function: UtoolsAiFunction {
                    name: "getSystemInfo".to_owned(),
                    description: "Read bounded plugin-owned information".to_owned(),
                    parameters: json!({ "type": "object", "properties": {} }),
                    required: Vec::new(),
                },
            }],
        };
        assert!(validate_ai_option(option).is_ok());
    }

    #[test]
    fn tool_calls_require_object_arguments_and_build_followup_wire_message() {
        let mut calls = BTreeMap::new();
        calls.insert(
            0,
            ToolCallBuilder {
                id: "call_1".to_owned(),
                name: "getSystemInfo".to_owned(),
                arguments: "{}".to_owned(),
            },
        );
        let round = finish_round(String::new(), String::new(), calls).expect("tool call");
        assert_eq!(round.tool_calls[0].arguments, json!({}));
        assert_eq!(
            round
                .assistant_wire_message
                .pointer("/tool_calls/0/function/name"),
            Some(&json!("getSystemInfo"))
        );
    }

    #[test]
    fn non_streaming_response_preserves_reasoning_and_tool_calls() {
        let mut chunks = Vec::new();
        let mut on_chunk = |message| {
            chunks.push(message);
            Ok(())
        };
        let round = parse_non_streaming_response(
            json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": "I will inspect it.",
                        "reasoning_content": "Need the plugin-owned value.",
                        "tool_calls": [{
                            "id": "call_2",
                            "type": "function",
                            "function": {
                                "name": "getSystemInfo",
                                "arguments": "{\"detail\":true}"
                            }
                        }]
                    }
                }]
            }),
            Some(&mut on_chunk),
        )
        .expect("valid non-streaming response");

        assert_eq!(chunks.len(), 1);
        assert_eq!(round.message.content.as_deref(), Some("I will inspect it."));
        assert_eq!(
            round.message.reasoning_content.as_deref(),
            Some("Need the plugin-owned value.")
        );
        assert_eq!(round.tool_calls[0].name, "getSystemInfo");
        assert_eq!(round.tool_calls[0].arguments, json!({ "detail": true }));
    }

    #[test]
    fn streaming_response_merges_text_reasoning_and_fragmented_tool_call() {
        let mut content = String::new();
        let mut reasoning = String::new();
        let mut tools = BTreeMap::new();
        let mut chunks = Vec::new();
        let mut on_chunk = |message| {
            chunks.push(message);
            Ok(())
        };
        for line in [
            br#"data: {"choices":[{"delta":{"reasoning_content":"Need ","tool_calls":[{"index":0,"id":"call_3","function":{"name":"getSystem","arguments":"{\"detail\":"}}]}}]}"#.as_slice(),
            br#"data: {"choices":[{"delta":{"content":"Checking ","reasoning":"context.","tool_calls":[{"index":0,"function":{"name":"Info","arguments":"true}"}}]}}]}"#.as_slice(),
            br#"data: {"choices":[{"delta":{"content":"done."}}]}"#.as_slice(),
            b"data: [DONE]".as_slice(),
        ] {
            process_sse_line(
                line,
                &mut on_chunk,
                &mut content,
                &mut reasoning,
                &mut tools,
            )
            .expect("valid stream event");
        }

        let round = finish_round(content, reasoning, tools).expect("merged stream response");
        assert_eq!(chunks.len(), 3);
        assert_eq!(round.message.content.as_deref(), Some("Checking done."));
        assert_eq!(
            round.message.reasoning_content.as_deref(),
            Some("Need context.")
        );
        assert_eq!(round.tool_calls[0].id, "call_3");
        assert_eq!(round.tool_calls[0].name, "getSystemInfo");
        assert_eq!(round.tool_calls[0].arguments, json!({ "detail": true }));
    }
}
