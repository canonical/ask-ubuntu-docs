use std::sync::{Arc, Mutex};

use anyhow::Result;
use clap::{Parser, Subcommand};

mod cli;
mod conversation;
mod gui;
mod llm;
mod markdown;
mod prompts;
mod vectordb;

use conversation::{create_client, load_rag_with_spinner, Conversation, DEFAULT_OLLAMA_URL};
use llm::keyring_accessible;

// Instruction given to the LLM at the start of every conversation
const SYSTEM_PROMPT: &str = include_str!("../cli-system-prompt.md");

// Top-level CLI struct; clap uses the fields and attributes to build argument parsing
#[derive(Parser)]
#[command(name = "ask-ubuntu-docs", about = "An LLM-based help app that uses the official Ubuntu documentation")]
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
            let client = create_client(ollama_url, cli.model, cli.copilot).await?;
            let rag = load_rag_with_spinner().await?;
            cli::run(client, rag, SYSTEM_PROMPT).await
        }
        Commands::Gui { ollama_url } => {
            // Warn in the GUI if we're inside a snap and the keyring plug isn't connected.
            // Only relevant for Copilot mode; Ollama needs no credentials.
            let show_keyring_warning = cli.copilot
                && std::env::var("SNAP").is_ok()
                && !keyring_accessible().await;
            let client = create_client(ollama_url, cli.model, cli.copilot).await?;
            let rag = load_rag_with_spinner().await?;
            let conversation = Arc::new(Mutex::new(Conversation::new(SYSTEM_PROMPT.to_string())));
            let tokio_handle = tokio::runtime::Handle::current();
            gui::run(Arc::new(client), Arc::new(Mutex::new(rag)), conversation, tokio_handle, show_keyring_warning)
        }
    }
}
