use std::io::{self, Write};
use std::process::Command as StdCommand;

use anyhow::{Context, Result};
use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};

// Model used when --copilot is selected; claude-haiku-4.5 is fast and available on all Copilot plans
const COPILOT_MODEL: &str = "claude-haiku-4.5";
// GitHub Copilot API endpoint — supports the full range of Copilot-licensed models including Claude
const COPILOT_API_URL: &str = "https://api.githubcopilot.com/chat/completions";

// A single message in a conversation, following the OpenAI/Ollama chat format
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Message {
    // Either "system", "user", or "assistant"
    pub role: String,
    pub content: String,
}

// ── Ollama types ─────────────────────────────────────────────────────────────

// The JSON body sent to Ollama's /api/chat endpoint
#[derive(Serialize)]
struct OllamaChatRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    // When true, Ollama sends one JSON object per line as tokens are generated
    stream: bool,
}

// One line of the streaming response from Ollama's /api/chat endpoint
#[derive(Deserialize)]
struct OllamaStreamChunk {
    // Partial message; content is a single token or small group of tokens
    message: Message,
    // True on the final chunk, indicating the response is complete
    done: bool,
}

// ── OpenAI / Copilot types ────────────────────────────────────────────────────

// The JSON body sent to the Copilot/OpenAI chat completions endpoint
#[derive(Serialize)]
struct OpenAiChatRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    stream: bool,
}

// One SSE event from the Copilot/OpenAI streaming response
#[derive(Deserialize)]
struct OpenAiChunk {
    choices: Vec<OpenAiChoice>,
}

#[derive(Deserialize)]
struct OpenAiChoice {
    // Delta carries the incremental content for this token
    delta: OpenAiDelta,
}

#[derive(Deserialize, Default)]
struct OpenAiDelta {
    // Null on the final chunk (finish_reason="stop"); absent on the first chunk (role-only)
    #[serde(default)]
    content: Option<String>,
}

// ── Ollama client ─────────────────────────────────────────────────────────────

// HTTP client wrapping the Ollama REST API
pub struct OllamaClient {
    // reqwest's async HTTP client; reusing one instance is more efficient than creating per-request
    client: Client,
    // Base URL of the Ollama server, e.g. "http://localhost:11434"
    url: String,
    // Name of the model to use, e.g. "tinyllama"
    model: String,
}

impl OllamaClient {
    // Creates a new client; does not open any network connections yet
    pub fn new(url: String, model: String) -> Self {
        Self {
            client: Client::new(),
            url,
            model,
        }
    }

    async fn send_request(&self, messages: &[Message]) -> Result<reqwest::Response> {
        let req = OllamaChatRequest { model: &self.model, messages, stream: true };
        let endpoint = format!("{}/api/chat", self.url.trim_end_matches('/'));
        Ok(self.client.post(&endpoint).json(&req).send().await?.error_for_status()?)
    }

    // Streams the reply token-by-token via Ollama's NDJSON format, printing to stdout.
    // `on_first_token` fires once just before the first character is printed.
    pub async fn chat<F: FnOnce()>(
        &self,
        messages: &[Message],
        on_first_token: F,
    ) -> Result<String> {
        let response = self.send_request(messages).await?;
        let result = stream_ndjson(
            response.bytes_stream(),
            on_first_token,
            parse_ollama_chunk,
            |t| {
                print!("{t}");
                io::stdout().flush().ok();
            },
        )
        .await;
        if result.is_ok() {
            println!();
        }
        result
    }

    // Like `chat` but calls `on_token` for each streaming token instead of printing to stdout.
    // Intended for GUI use where tokens are routed to a widget rather than the terminal.
    pub async fn chat_streaming<F, G>(
        &self,
        messages: &[Message],
        on_first_token: F,
        on_token: G,
    ) -> Result<String>
    where
        F: FnOnce(),
        G: Fn(&str),
    {
        let response = self.send_request(messages).await?;
        stream_ndjson(response.bytes_stream(), on_first_token, parse_ollama_chunk, on_token).await
    }
}

fn parse_ollama_chunk(line: &str) -> Result<StreamToken> {
    let chunk: OllamaStreamChunk =
        serde_json::from_str(line).context("failed to parse Ollama stream chunk")?;
    Ok(StreamToken { content: chunk.message.content, done: chunk.done })
}

// ── Copilot client ─────────────────────────────────────────────────────────────

// HTTP client wrapping the GitHub Models API (https://models.inference.ai.azure.com)
pub struct CopilotClient {
    client: Client,
    // GitHub PAT or OAuth token; used directly as a Bearer token with the GitHub Models API
    token: String,
}

impl CopilotClient {
    // Resolves the GitHub token using one of two methods (in order):
    //   1. COPILOT_TOKEN env var — use directly
    //   2. `gh auth token` — reads the token of the currently logged-in gh CLI user
    pub async fn create() -> Result<Self> {
        let http = Client::new();

        // Allow the user to supply a token directly, bypassing the gh CLI
        if let Ok(token) = std::env::var("COPILOT_TOKEN") {
            if !token.trim().is_empty() {
                eprintln!("Using token from COPILOT_TOKEN environment variable.");
                return Ok(Self { client: http, token: token.trim().to_string() });
            }
        }

        // Fall back to reading the token from the gh CLI
        let gh_output = StdCommand::new("gh")
            .args(["auth", "token"])
            .output()
            .context("failed to run `gh auth token`; install the GitHub CLI and run `gh auth login`")?;

        if !gh_output.status.success() {
            let stderr = String::from_utf8_lossy(&gh_output.stderr);
            anyhow::bail!("`gh auth token` failed: {stderr}");
        }

        let token = String::from_utf8(gh_output.stdout)
            .context("`gh auth token` output is not valid UTF-8")?
            .trim()
            .to_string();

        Ok(Self { client: http, token })
    }

    async fn send_request(&self, messages: &[Message]) -> Result<reqwest::Response> {
        let req = OpenAiChatRequest { model: COPILOT_MODEL, messages, stream: true };
        let response = self
            .client
            .post(COPILOT_API_URL)
            .header("Authorization", format!("Bearer {}", self.token))
            .json(&req)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Copilot API error {status}: {body}");
        }

        Ok(response)
    }

    // Streams the reply token-by-token via the GitHub Models OpenAI-compatible SSE API,
    // printing to stdout. `on_first_token` fires once just before the first character is printed.
    pub async fn chat<F: FnOnce()>(
        &self,
        messages: &[Message],
        on_first_token: F,
    ) -> Result<String> {
        let response = self.send_request(messages).await?;
        let result = stream_ndjson(
            response.bytes_stream(),
            on_first_token,
            parse_copilot_chunk,
            |t| {
                print!("{t}");
                io::stdout().flush().ok();
            },
        )
        .await;
        if result.is_ok() {
            println!();
        }
        result
    }

    // Like `chat` but calls `on_token` for each streaming token instead of printing to stdout.
    pub async fn chat_streaming<F, G>(
        &self,
        messages: &[Message],
        on_first_token: F,
        on_token: G,
    ) -> Result<String>
    where
        F: FnOnce(),
        G: Fn(&str),
    {
        let response = self.send_request(messages).await?;
        stream_ndjson(response.bytes_stream(), on_first_token, parse_copilot_chunk, on_token).await
    }
}

fn parse_copilot_chunk(line: &str) -> Result<StreamToken> {
    // SSE lines that don't carry data (comments, id:, event:) are valid and must be skipped.
    let Some(json) = line.strip_prefix("data: ") else {
        return Ok(StreamToken { content: String::new(), done: false });
    };
    if json.is_empty() {
        return Ok(StreamToken { content: String::new(), done: false });
    }
    if json == "[DONE]" {
        return Ok(StreamToken { content: String::new(), done: true });
    }
    let chunk: OpenAiChunk = serde_json::from_str(json)
        .context("failed to parse Copilot SSE chunk")?;
    let content = chunk.choices.into_iter().next()
        .and_then(|c| c.delta.content)
        .unwrap_or_default();
    Ok(StreamToken { content, done: false })
}

// ── Unified client enum ───────────────────────────────────────────────────────

// Dispatches chat calls to either the local Ollama backend or the Copilot cloud backend
pub enum LlmClient {
    Ollama(OllamaClient),
    Copilot(CopilotClient),
}

impl LlmClient {
    // For CLI: streams tokens to stdout.
    pub async fn chat<F: FnOnce()>(
        &self,
        messages: &[Message],
        on_first_token: F,
    ) -> Result<String> {
        match self {
            LlmClient::Ollama(c) => c.chat(messages, on_first_token).await,
            LlmClient::Copilot(c) => c.chat(messages, on_first_token).await,
        }
    }

    // For GUI: calls `on_token` for each token instead of writing to stdout.
    pub async fn chat_streaming<F, G>(
        &self,
        messages: &[Message],
        on_first_token: F,
        on_token: G,
    ) -> Result<String>
    where
        F: FnOnce(),
        G: Fn(&str),
    {
        match self {
            LlmClient::Ollama(c) => c.chat_streaming(messages, on_first_token, on_token).await,
            LlmClient::Copilot(c) => c.chat_streaming(messages, on_first_token, on_token).await,
        }
    }
}

// ── Shared streaming helper ───────────────────────────────────────────────────

// Carries one decoded token from either streaming format
struct StreamToken {
    content: String,
    // When true, the stream is finished
    done: bool,
}

// Reads a byte stream line-by-line, calls `parse_line` on each non-empty line,
// calls `on_token` for each non-empty content token, and returns the full assembled reply.
// `on_first_token` is called once before the first non-empty token.
async fn stream_ndjson<S, E, F, P, G>(
    mut stream: S,
    on_first_token: F,
    parse_line: P,
    on_token: G,
) -> Result<String>
where
    S: futures_util::Stream<Item = Result<bytes::Bytes, E>> + Unpin,
    E: std::error::Error + Send + Sync + 'static,
    F: FnOnce(),
    P: Fn(&str) -> Result<StreamToken>,
    G: Fn(&str),
{
    let mut full_reply = String::new();
    let mut buf = Vec::new();
    let mut on_first_token = Some(on_first_token);

    while let Some(chunk) = stream.next().await {
        let bytes = chunk.context("error reading stream chunk")?;
        buf.extend_from_slice(&bytes);

        // Process every complete newline-terminated line in the buffer
        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let line_bytes = buf.drain(..=pos).collect::<Vec<_>>();
            let line = String::from_utf8_lossy(&line_bytes);
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let token = parse_line(line)?;

            if !token.content.is_empty() {
                if let Some(f) = on_first_token.take() {
                    f();
                }
                on_token(&token.content);
                full_reply.push_str(&token.content);
            }

            if token.done {
                return Ok(full_reply);
            }
        }
    }

    Ok(full_reply)
}

