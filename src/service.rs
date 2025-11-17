use std::env;
use std::path::Path;
use std::time::Duration;

use reqwest::Client;
use rmcp::handler::server::{tool::ToolRouter, wrapper::Parameters};
use rmcp::model::{CallToolResult, Content, ErrorData as McpError, ServerCapabilities, ServerInfo};
use rmcp::ServiceExt;
use rmcp::{tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use tokio::time::sleep;

const MAX_PROMPT_CHARS: usize = 1_000_000;
const OPENAI_POLL_TIMEOUT_SECS: u64 = 120;
const OPENAI_POLL_START_DELAY_MS: u64 = 500;
const OPENAI_POLL_MAX_DELAY_MS: u64 = 5_000;
const OPENAI_JSON_PREVIEW_CHARS: usize = 2_000;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct OracleRequest {
    /// Natural-language description of the coding problem you want help with.
    pub problem: String,
    /// List of file paths to include as context. Paths are resolved relative to the working dir.
    pub files: Option<Vec<String>>,
    /// Optional extra context or notes.
    pub extra_context: Option<String>,
}

#[derive(Clone)]
pub struct OracleService {
    tool_router: ToolRouter<OracleService>,
    http: Client,
}

impl OracleService {
    pub fn new() -> Self {
        let http = Client::builder()
            .user_agent("oracle-mcp-server/0.1")
            .build()
            .expect("failed to build HTTP client");

        Self {
            tool_router: Self::tool_router(),
            http,
        }
    }

    fn test_mode_enabled() -> bool {
        match env::var("ORACLE_TEST_MODE") {
            Ok(value) => {
                let normalized = value.trim().to_ascii_lowercase();
                matches!(normalized.as_str(), "1" | "true" | "yes" | "on")
            }
            Err(_) => false,
        }
    }

    fn test_mode_response(request: &OracleRequest) -> String {
        let mut response = String::from(
            "[oracle test mode] No call was made to OpenAI because ORACLE_TEST_MODE is set.\n\n",
        );

        response.push_str("Problem description:\n");
        response.push_str(&request.problem);
        response.push_str("\n\n");

        match &request.extra_context {
            Some(extra) if !extra.trim().is_empty() => {
                response.push_str("Extra context:\n");
                response.push_str(extra);
                response.push_str("\n\n");
            }
            _ => {
                response.push_str("Extra context: (not provided)\n\n");
            }
        }

        response.push_str("Files provided:\n");
        match &request.files {
            Some(files) if !files.is_empty() => {
                for path in files {
                    response.push_str("- ");
                    response.push_str(path);
                    response.push('\n');
                }
            }
            _ => response.push_str("(none)\n"),
        }

        response
    }

    pub async fn call_openai(&self, request: OracleRequest) -> Result<String, McpError> {
        if Self::test_mode_enabled() {
            return Ok(Self::test_mode_response(&request));
        }

        let user_prompt = build_prompt(&request).await;

        let api_key = env::var("OPENAI_API_KEY").map_err(|_| {
            McpError::internal_error("Environment variable OPENAI_API_KEY is not set", None)
        })?;

        // Build Responses API request for gpt-5-pro with high reasoning effort.
        #[derive(serde::Serialize, Clone)]
        struct Reasoning {
            effort: String,
        }

        #[derive(serde::Serialize, Clone)]
        struct ResponseRequest {
            model: String,
            input: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            instructions: Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            reasoning: Option<Reasoning>,
            #[serde(skip_serializing_if = "Option::is_none")]
            max_output_tokens: Option<u32>,
        }

        let mut max_output_tokens: u32 = 2048;
        let mut attempts = 0u8;

        loop {
            attempts += 1;

            let body = ResponseRequest {
                model: "gpt-5-pro".to_string(),
                input: user_prompt.clone(),
                instructions: Some(
                    "You are Oracle, a meticulous, senior-level coding assistant. Always think step-by-step and consider edge cases before answering. When relevant, suggest concrete code changes and explain why.".to_string(),
                ),
                reasoning: Some(Reasoning {
                    effort: "high".to_string(),
                }),
                max_output_tokens: Some(max_output_tokens),
            };

            let resp = self
                .http
                .post("https://api.openai.com/v1/responses")
                .bearer_auth(&api_key)
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
                .map_err(|err| {
                    McpError::internal_error(format!("Failed to call OpenAI API: {err}"), None)
                })?;

            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                return Err(McpError::internal_error(
                    format!("OpenAI API returned non-success status {status}: {text}"),
                    None,
                ));
            }

            let initial_response: Value = resp.json().await.map_err(|err| {
                McpError::internal_error(format!("Failed to parse OpenAI response: {err}"), None)
            })?;

            let completed_response = self
                .wait_for_openai_completion(initial_response, &api_key)
                .await?;

            let status = response_status(&completed_response).unwrap_or("unknown");
            let answer = extract_output_text(&completed_response);

            if let Some(mut answer) = answer {
                if status == "incomplete" {
                    let reason = incomplete_reason(&completed_response)
                        .unwrap_or_else(|| "reason unavailable".to_string());
                    answer.push_str(&format!(
                        "\n\n[oracle warning] OpenAI stopped early ({reason}). The answer may be truncated.",
                    ));
                }
                return Ok(answer);
            }

            if status == "incomplete"
                && incomplete_reason(&completed_response).as_deref() == Some("max_output_tokens")
                && max_output_tokens < 8192
                && attempts < 3
            {
                max_output_tokens = (max_output_tokens * 2).min(8192);
                continue;
            }

            if status == "incomplete" {
                let reason = incomplete_reason(&completed_response)
                    .unwrap_or_else(|| "reason unavailable".to_string());
                return Err(McpError::internal_error(
                    format!(
                        "OpenAI response ended incomplete ({reason}) before returning any text. Raw payload: {}",
                        summarize_json(&completed_response)
                    ),
                    None,
                ));
            }

            return Err(McpError::internal_error(
                format!(
                    "OpenAI response did not contain any text output. Raw payload: {}",
                    summarize_json(&completed_response)
                ),
                None,
            ));
        }
    }

    async fn wait_for_openai_completion(
        &self,
        mut response_json: Value,
        api_key: &str,
    ) -> Result<Value, McpError> {
        let response_id = response_json
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                McpError::internal_error(
                    format!(
                        "OpenAI response missing an id. Raw payload: {}",
                        summarize_json(&response_json)
                    ),
                    None,
                )
            })?
            .to_string();

        let mut delay = Duration::from_millis(OPENAI_POLL_START_DELAY_MS);
        let mut elapsed = Duration::ZERO;

        loop {
            let status = response_status(&response_json).unwrap_or("unknown");

            match status {
                "completed" | "incomplete" => return Ok(response_json),
                "failed" => {
                    let message = openai_error_message(&response_json)
                        .unwrap_or_else(|| "OpenAI response marked as failed".to_string());
                    return Err(McpError::internal_error(
                        format!("{message}. Raw payload: {}", summarize_json(&response_json)),
                        None,
                    ));
                }
                "requires_action" => {
                    return Err(McpError::internal_error(
                        format!(
                            "OpenAI response requires additional action that Oracle cannot perform. Raw payload: {}",
                            summarize_json(&response_json)
                        ),
                        None,
                    ));
                }
                "cancelled" => {
                    return Err(McpError::internal_error(
                        format!(
                            "OpenAI response was cancelled before completion. Raw payload: {}",
                            summarize_json(&response_json)
                        ),
                        None,
                    ));
                }
                status if should_poll_status(status) => {
                    if elapsed >= Duration::from_secs(OPENAI_POLL_TIMEOUT_SECS) {
                        return Err(McpError::internal_error(
                            format!(
                                "Timed out waiting for OpenAI response {response_id} to finish. Last payload: {}",
                                summarize_json(&response_json)
                            ),
                            None,
                        ));
                    }

                    sleep(delay).await;
                    elapsed += delay;
                    delay = next_poll_delay(delay);

                    response_json = self
                        .http
                        .get(format!("https://api.openai.com/v1/responses/{response_id}"))
                        .bearer_auth(api_key)
                        .send()
                        .await
                        .map_err(|err| {
                            McpError::internal_error(
                                format!("Failed to poll OpenAI response: {err}"),
                                None,
                            )
                        })?
                        .json()
                        .await
                        .map_err(|err| {
                            McpError::internal_error(
                                format!("Failed to parse OpenAI poll response: {err}"),
                                None,
                            )
                        })?;
                }
                other => {
                    return Err(McpError::internal_error(
                        format!(
                            "OpenAI response entered unexpected status '{other}'. Raw payload: {}",
                            summarize_json(&response_json)
                        ),
                        None,
                    ));
                }
            }
        }
    }
}

pub async fn run_server() -> Result<(), Box<dyn std::error::Error>> {
    let service = OracleService::new();
    let running = service.serve(rmcp::transport::stdio()).await?;
    let _ = running.waiting().await;
    Ok(())
}

async fn build_prompt(request: &OracleRequest) -> String {
    let request = request.clone();
    let mut context_blocks = String::new();

    if let Some(files) = &request.files {
        for path in files {
            let path_obj = Path::new(path);
            let display = path_obj.display();

            match tokio::fs::read_to_string(path_obj).await {
                Ok(contents) => {
                    context_blocks
                        .push_str(&format!("\n\n===== FILE: {display} =====\n{contents}\n",));
                }
                Err(err) => {
                    context_blocks.push_str(&format!(
                        "\n\n===== FILE: {display} (error reading) =====\n{err}\n",
                    ));
                }
            }
        }
    }

    let mut user_prompt = String::new();
    user_prompt.push_str("You are Oracle, a senior software engineer MCP tool.\n");
    user_prompt.push_str("You will be given a coding problem and optional project files.\n");
    user_prompt.push_str("Carefully analyze the problem, read the files, reason step-by-step, and produce a clear, actionable answer.\n\n");
    user_prompt.push_str("Context is constrained to stay under roughly 256k tokens. If you see '[truncated]' markers, some content was cut to fit the budget.\n\n");

    user_prompt.push_str("### Coding problem\n");
    user_prompt.push_str(&request.problem);
    user_prompt.push_str("\n\n");

    if let Some(extra) = &request.extra_context {
        user_prompt.push_str("### Extra context\n");
        user_prompt.push_str(extra);
        user_prompt.push_str("\n\n");
    }

    if !context_blocks.is_empty() {
        let header = "### Project files\n";
        let trunc_notice =
            "\n\n...[truncated project file content to respect ~256k-token context budget]...\n";

        let base_len = user_prompt.len() + header.len() + trunc_notice.len();
        let available_for_files = MAX_PROMPT_CHARS.saturating_sub(base_len);

        user_prompt.push_str(header);

        if available_for_files == 0 {
            user_prompt.push_str(trunc_notice);
        } else if context_blocks.len() as usize > available_for_files {
            let mut truncated = context_blocks;
            truncated.truncate(available_for_files);
            user_prompt.push_str(&truncated);
            user_prompt.push_str(trunc_notice);
        } else {
            user_prompt.push_str(&context_blocks);
        }
    }

    user_prompt
}

fn response_status(value: &Value) -> Option<&str> {
    value.get("status").and_then(|v| v.as_str())
}

fn should_poll_status(status: &str) -> bool {
    matches!(status, "queued" | "in_progress" | "cancelling")
}

fn next_poll_delay(current: Duration) -> Duration {
    let mut millis = current.as_millis() as u64;
    if millis == 0 {
        millis = OPENAI_POLL_START_DELAY_MS;
    } else {
        millis = millis + millis / 2;
    }
    millis = millis.min(OPENAI_POLL_MAX_DELAY_MS);
    Duration::from_millis(millis)
}

fn openai_error_message(value: &Value) -> Option<String> {
    value
        .get("error")
        .and_then(|err| err.get("message"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn incomplete_reason(value: &Value) -> Option<String> {
    value
        .get("incomplete_details")
        .and_then(|v| v.get("reason"))
        .and_then(|v| v.as_str())
        .map(|reason| reason.to_string())
}

fn extract_output_text(response: &Value) -> Option<String> {
    if let Some(text) = response.get("output_text").and_then(|v| v.as_str()) {
        let text = text.trim();
        if !text.is_empty() {
            return Some(text.to_string());
        }
    }

    if let Some(output_text) = response.get("output_text").and_then(|v| v.as_array()) {
        let mut buffer = String::new();
        for chunk in output_text.iter().filter_map(|v| v.as_str()) {
            append_text_segment(&mut buffer, chunk);
        }
        if !buffer.is_empty() {
            return Some(buffer);
        }
    }

    if let Some(output_items) = response.get("output").and_then(|v| v.as_array()) {
        let mut buffer = String::new();
        for item in output_items {
            if let Some(content) = item.get("content").and_then(|v| v.as_array()) {
                collect_text_from_contents(content, &mut buffer);
            }
        }
        if !buffer.is_empty() {
            return Some(buffer);
        }
    }

    if let Some(content) = response.get("content").and_then(|v| v.as_array()) {
        let mut buffer = String::new();
        collect_text_from_contents(content, &mut buffer);
        if !buffer.is_empty() {
            return Some(buffer);
        }
    }

    None
}

fn collect_text_from_contents(contents: &[Value], buffer: &mut String) {
    for entry in contents {
        if let Some(text) = entry.get("text").and_then(|v| v.as_str()) {
            append_text_segment(buffer, text);
        }
        if let Some(nested) = entry.get("content").and_then(|v| v.as_array()) {
            collect_text_from_contents(nested, buffer);
        }
    }
}

fn append_text_segment(buffer: &mut String, text: &str) {
    if text.trim().is_empty() {
        return;
    }
    if !buffer.is_empty() {
        buffer.push_str("\n\n");
    }
    buffer.push_str(text);
}

fn summarize_json(value: &Value) -> String {
    let json_str = value.to_string();
    if json_str.len() <= OPENAI_JSON_PREVIEW_CHARS {
        return json_str;
    }

    format!(
        "{}...[truncated {} chars]",
        &json_str[..OPENAI_JSON_PREVIEW_CHARS],
        json_str.len() - OPENAI_JSON_PREVIEW_CHARS
    )
}

#[tool_router]
impl OracleService {
    #[tool(
        name = "solve_coding_problem",
        description = "Analyze a difficult coding problem (optionally using local project files) and return a detailed solution and suggested code changes.",
        annotations(
            title = "Oracle: Solve coding problems",
            read_only_hint = true,
            idempotent_hint = true
        )
    )]
    async fn oracle(
        &self,
        Parameters(request): Parameters<OracleRequest>,
    ) -> Result<CallToolResult, McpError> {
        match self.call_openai(request).await {
            Ok(answer) => Ok(CallToolResult::success(vec![Content::text(answer)])),
            Err(err) => Ok(CallToolResult::error(vec![Content::text(format!(
                "Oracle encountered an error: {}",
                err.message
            ))])),
        }
    }
}

#[tool_handler]
impl rmcp::ServerHandler for OracleService {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            instructions: Some(
                "Oracle is a coding-focused MCP server that uses OpenAI's gpt-5-pro model with high reasoning to answer questions about your code. Use the `solve_coding_problem` tool with a coding problem and optional file paths; it will analyze the problem and files and propose concrete fixes.".into(),
            ),
            ..Default::default()
        }
    }
}
