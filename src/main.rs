use std::io::{self, BufRead, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand};
use indicatif::{ProgressBar, ProgressStyle};

// Import the LLM module defined in llm.rs
mod llm;
mod markdown;
mod prompts;
use llm::{CopilotClient, DEFAULT_COPILOT_MODEL, LlmClient, Message, OllamaClient};

// Import the in-memory RAG module defined in vectordb.rs
mod vectordb;
use vectordb::{RagStore, TOP_K};

mod gui;

// Default address where Ollama listens when installed locally
const DEFAULT_OLLAMA_URL: &str = "http://localhost:11434";
// Default model to use; small enough to run without a GPU
const DEFAULT_MODEL: &str = "deepseek-r1:1.5b";
// Instruction given to the LLM at the start of every conversation
const SYSTEM_PROMPT: &str = include_str!("../cli-system-prompt.md");

// Top-level CLI struct; clap uses the fields and attributes to build argument parsing
#[derive(Parser)]
#[command(name = "ubuntu-desktop-help", about = "Ubuntu Desktop Help CLI")]
struct Cli {
    // Model name to use. For Ollama (default backend): e.g. "tinyllama", "phi3:mini".
    // For --copilot: any model available on your plan, e.g. "gpt-4o-mini",
    // "claude-sonnet-4.5". Defaults to deepseek-r1:1.5b for Ollama and
    // gpt-4o-mini for Copilot when not specified.
    #[arg(long, env = "MODEL", global = true)]
    model: Option<String>,

    // Use GitHub Copilot instead of a local Ollama model
    #[arg(long, global = true)]
    copilot: bool,

    #[command(subcommand)]
    command: Commands,
}

// All available subcommands
#[derive(Subcommand)]
enum Commands {
    /// Start an interactive chat session in the terminal
    Chat {
        // Ollama server URL; only used when --copilot is not set
        #[arg(long, env = "OLLAMA_URL", default_value = DEFAULT_OLLAMA_URL)]
        ollama_url: String,
    },
    /// Launch the graphical user interface
    Gui {
        #[arg(long, env = "OLLAMA_URL", default_value = DEFAULT_OLLAMA_URL)]
        ollama_url: String,
    },
}

// Entry point; #[tokio::main] sets up the async runtime so we can use .await
#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Chat { ollama_url } => {
            run_chat(ollama_url, cli.model, cli.copilot).await
        }
        Commands::Gui { ollama_url } => {
            run_gui(ollama_url, cli.model, cli.copilot).await
        }
    }
}

// Runs the interactive chat loop, sending user input to the chosen LLM backend and printing replies
async fn run_chat(ollama_url: String, model: Option<String>, use_copilot: bool) -> Result<()> {
    // Build the appropriate LLM backend based on whether --copilot was passed
    let client = if use_copilot {
        let copilot_model = model.unwrap_or_else(|| DEFAULT_COPILOT_MODEL.to_string());
        eprintln!("Authenticating with GitHub Copilot (model: {copilot_model})…");
        LlmClient::Copilot(CopilotClient::create(copilot_model).await?)
    } else {
        let ollama_model = model.unwrap_or_else(|| DEFAULT_MODEL.to_string());
        LlmClient::Ollama(OllamaClient::new(ollama_url, ollama_model))
    };

    // Load the RAG index (embedding model init is blocking; block_in_place lets tokio
    // keep running other tasks on the remaining threads while this thread blocks)
    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner} Loading model…")
            .unwrap(),
    );
    spinner.enable_steady_tick(Duration::from_millis(80));
    let mut rag = RagStore::load().await?;
    spinner.finish_and_clear();

    // history holds bare user queries + assistant replies only — no RAG chunks, no system prompt.
    // The system prompt and per-turn RAG context are injected fresh each call so they appear
    // exactly once regardless of how many turns the conversation has had.
    let system_msg = Message {
        role: "system".to_string(),
        content: SYSTEM_PROMPT.to_string(),
    };
    let mut history: Vec<Message> = vec![];

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    loop {
        // Print prompt and flush immediately so it appears before the user types
        print!("> ");
        stdout.flush()?;

        // Read one line inside a block so the StdinLock is dropped before any .await calls
        let input = {
            let mut line = String::new();
            match stdin.lock().read_line(&mut line) {
                Ok(0) => break, // EOF (Ctrl-D)
                Ok(_) => line.trim().to_string(),
                Err(e) => {
                    eprintln!("Error reading input: {e}");
                    break;
                }
            }
        };

        if input.is_empty() {
            continue;
        }
        if input.eq_ignore_ascii_case("exit") {
            break;
        }

        // Retrieve the most relevant doc chunks for this query via hybrid search
        let query_vec = rag.embed(&input)?;
        let relevant = RagStore::search_with_vec(&rag.table, &input, query_vec, TOP_K).await?;

        // Build the augmented user message for this turn only — not stored in history.
        // Keeping RAG chunks out of history ensures the doc context doesn't accumulate
        // and re-inflate the prompt on every subsequent turn.
        let user_content = if relevant.is_empty() {
            input.clone()
        } else {
            let ctx = relevant
                .iter()
                .map(|(source, text)| format!("[Source: {source}]\n{text}"))
                .collect::<Vec<_>>()
                .join("\n\n");
            format!("Context from documentation:\n{ctx}\n\nQuestion: {input}")
        };

        // Assemble the full message list for this LLM call:
        // system prompt (once) + bare conversation history + augmented current turn
        let mut llm_messages = Vec::with_capacity(history.len() + 2);
        llm_messages.push(system_msg.clone());
        llm_messages.extend_from_slice(&history);
        llm_messages.push(Message { role: "user".to_string(), content: user_content });

        // Show a spinner while waiting for the first token from the LLM
        let spinner = ProgressBar::new_spinner();
        spinner.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner} Thinking…")
                .unwrap(),
        );
        spinner.enable_steady_tick(Duration::from_millis(80));

        // Pass a callback that clears the spinner the moment the first token arrives
        match client.chat(&llm_messages, || spinner.finish_and_clear()).await {
            Ok(reply) => {
                // Tokens were already printed by the streaming chat call; just add spacing
                println!();
                // Store bare query + reply in history for future turns
                history.push(Message { role: "user".to_string(), content: input });
                history.push(Message { role: "assistant".to_string(), content: reply });
            }
            Err(e) => {
                spinner.finish_and_clear();
                eprintln!("Error: {e}");
                // history is untouched — the failed turn leaves no trace
            }
        }
    }

    Ok(())
}

async fn run_gui(ollama_url: String, model: Option<String>, use_copilot: bool) -> Result<()> {
    let client = if use_copilot {
        let copilot_model = model.unwrap_or_else(|| DEFAULT_COPILOT_MODEL.to_string());
        eprintln!("Authenticating with GitHub Copilot (model: {copilot_model})…");
        LlmClient::Copilot(CopilotClient::create(copilot_model).await?)
    } else {
        let ollama_model = model.unwrap_or_else(|| DEFAULT_MODEL.to_string());
        LlmClient::Ollama(OllamaClient::new(ollama_url, ollama_model))
    };

    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner} Loading model…")
            .unwrap(),
    );
    spinner.enable_steady_tick(Duration::from_millis(80));
    let rag = RagStore::load().await?;
    spinner.finish_and_clear();

    let conversation = Arc::new(Mutex::new(vec![Message {
        role: "system".to_string(),
        content: SYSTEM_PROMPT.to_string(),
    }]));

    let tokio_handle = tokio::runtime::Handle::current();

    gui::run(Arc::new(client), Arc::new(Mutex::new(rag)), conversation, tokio_handle)
}
