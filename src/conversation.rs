use std::time::Duration;

use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};

use crate::llm::{CopilotClient, DEFAULT_COPILOT_MODEL, LlmClient, Message, OllamaClient, Role};
use crate::vectordb::RagStore;

// Default address where Ollama listens when installed locally
pub const DEFAULT_OLLAMA_URL: &str = "http://localhost:11434";
// Default model to use with Ollama; small enough to run without a GPU
pub const DEFAULT_MODEL: &str = "deepseek-r1:1.5b";

/// Encapsulates conversation history + system prompt for one chat session.
///
/// Both the CLI and GUI use this struct to manage state. The system prompt and
/// per-turn RAG context are NOT stored in history — they are assembled fresh for
/// each LLM call, so they appear exactly once regardless of how many turns have elapsed.
pub struct Conversation {
    system_prompt: String,
    // Bare user queries and assistant replies only — no RAG chunks, no system message.
    history: Vec<Message>,
}

impl Conversation {
    pub fn new(system_prompt: String) -> Self {
        Self { system_prompt, history: Vec::new() }
    }

    /// The last assistant reply in the history, if one exists.
    pub fn last_assistant_reply(&self) -> Option<&str> {
        self.history.iter().rev().find(|m| m.role == Role::Assistant).map(|m| m.content.as_str())
    }

    /// Build the RAG search query for the current user input.
    ///
    /// Prepends the last assistant reply (if any) so that ambiguous follow-up
    /// questions like "how do I install it?" retrieve the right topic.
    /// The bare input is still what gets stored in history.
    pub fn build_rag_query(&self, input: &str) -> String {
        match self.last_assistant_reply() {
            Some(prev) => format!("{prev}\n\n{input}"),
            None => input.to_string(),
        }
    }

    /// Format retrieved doc chunks + user question into the augmented user message
    /// that the LLM receives. The chunks are omitted from history — this message
    /// is for the current LLM call only.
    pub fn build_augmented_content(input: &str, chunks: &[(String, String)]) -> String {
        if chunks.is_empty() {
            return input.to_string();
        }
        let ctx = chunks
            .iter()
            .map(|(source, text)| format!("[Source: {source}]\n{text}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        format!("Context from documentation:\n{ctx}\n\nQuestion: {input}")
    }

    /// Assemble the full message list for one LLM call:
    /// `[system, ...history, augmented_current_turn]`.
    pub fn build_llm_messages(&self, augmented_user_content: String) -> Vec<Message> {
        let mut msgs = Vec::with_capacity(self.history.len() + 2);
        msgs.push(Message { role: Role::System, content: self.system_prompt.clone() });
        msgs.extend_from_slice(&self.history);
        msgs.push(Message { role: Role::User, content: augmented_user_content });
        msgs
    }

    /// Record a completed turn. Call this only after a successful LLM reply.
    /// Stores the bare user input — never the augmented content with RAG chunks.
    pub fn add_turn(&mut self, user_input: String, assistant_reply: String) {
        self.history.push(Message { role: Role::User, content: user_input });
        self.history.push(Message { role: Role::Assistant, content: assistant_reply });
    }
}

/// Build the appropriate LLM backend based on CLI flags.
pub async fn create_client(
    ollama_url: String,
    model: Option<String>,
    use_copilot: bool,
) -> Result<LlmClient> {
    if use_copilot {
        let copilot_model = model.unwrap_or_else(|| DEFAULT_COPILOT_MODEL.to_string());
        eprintln!("Authenticating with GitHub Copilot (model: {copilot_model})…");
        Ok(LlmClient::Copilot(CopilotClient::create(copilot_model).await?))
    } else {
        let ollama_model = model.unwrap_or_else(|| DEFAULT_MODEL.to_string());
        Ok(LlmClient::Ollama(OllamaClient::new(ollama_url, ollama_model)))
    }
}

/// Load the RAG index while showing a terminal spinner.
///
/// The embedding model initialisation is CPU-bound and can take a few seconds,
/// so a spinner keeps the user informed.
pub async fn load_rag_with_spinner() -> Result<RagStore> {
    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner} Loading model…")
            .unwrap(),
    );
    spinner.enable_steady_tick(Duration::from_millis(80));
    let rag = RagStore::load().await?;
    spinner.finish_and_clear();
    Ok(rag)
}
