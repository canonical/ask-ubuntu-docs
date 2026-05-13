use std::io::{self, BufRead, Write};
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand};
use indicatif::{ProgressBar, ProgressStyle};

// Import the LLM module defined in llm.rs
mod llm;
use llm::{CopilotClient, LlmClient, Message, OllamaClient};

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
    // Local Ollama model name (e.g. tinyllama, phi3:mini); mutually exclusive with --copilot
    #[arg(long, env = "OLLAMA_MODEL", default_value = DEFAULT_MODEL, global = true, conflicts_with = "copilot")]
    model: String,

    // Use GitHub Copilot via the GitHub Models API instead of a local model; mutually exclusive with --model
    #[arg(long, global = true, conflicts_with = "model")]
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
    Gui,
}

// Entry point; #[tokio::main] sets up the async runtime so we can use .await
#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Chat { ollama_url } => {
            run_chat(ollama_url, cli.model, cli.copilot).await
        }
        Commands::Gui => {
            run_gui()
        }
    }
}

// Runs the interactive chat loop, sending user input to the chosen LLM backend and printing replies
async fn run_chat(ollama_url: String, model: String, use_copilot: bool) -> Result<()> {
    // Build the appropriate LLM backend based on whether --copilot was passed
    let client = if use_copilot {
        eprintln!("Authenticating with GitHub Copilot…");
        LlmClient::Copilot(CopilotClient::create().await?)
    } else {
        LlmClient::Ollama(OllamaClient::new(ollama_url, model))
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
    let mut rag = tokio::task::block_in_place(RagStore::load)?;
    spinner.finish_and_clear();

    // System prompt contains only role instructions; doc context is injected per-query via RAG
    let mut messages = vec![Message {
        role: "system".to_string(),
        content: SYSTEM_PROMPT.to_string(),
    }];

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

        // Retrieve the most relevant doc chunks for this query via cosine similarity
        let relevant = tokio::task::block_in_place(|| rag.search(&input, TOP_K))?;

        // Prepend retrieved chunks as context so the LLM answers from documentation
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

        messages.push(Message {
            role: "user".to_string(),
            content: user_content,
        });

        // Show a spinner while waiting for the first token from the LLM
        let spinner = ProgressBar::new_spinner();
        spinner.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner} Thinking…")
                .unwrap(),
        );
        spinner.enable_steady_tick(Duration::from_millis(80));

        // Pass a callback that clears the spinner the moment the first token arrives
        match client.chat(&messages, || spinner.finish_and_clear()).await {
            Ok(reply) => {
                // Tokens were already printed by the streaming chat call; just add spacing
                println!();
                // Store the assistant reply so future turns have full context
                messages.push(Message {
                    role: "assistant".to_string(),
                    content: reply,
                });
            }
            Err(e) => {
                spinner.finish_and_clear();
                eprintln!("Error: {e}");
                // Remove the user message to keep history consistent with what the LLM has seen
                messages.pop();
            }
        }
    }

    Ok(())
}

fn run_gui() -> Result<()> {
    gui::run()
}
