# spec.md — Ask the Docs

## Purpose

An AI-powered assistant that answers questions about Ubuntu. The user asks a natural-language question; the app retrieves relevant chunks from the official Ubuntu documentation using hybrid search, feeds them to an LLM, and returns a streamed answer.

The app ships as a snap. It has a GTK4 graphical interface and an interactive terminal chat mode. It supports a local Ollama backend and the GitHub Models API (via GitHub Copilot).

---

## Architecture

```
┌─────────────────────────────────────────────────────┐
│                   User Interface                    │
│         GTK4 GUI (gui.rs)  /  Terminal (cli.rs)     │
└───────────────────┬─────────────────────────────────┘
                    │ query
┌───────────────────▼─────────────────────────────────┐
│                  RAG Pipeline                       │
│  1. Embed query (fastembed, BGE-small-en-v1.5)      │
│  2. Hybrid search: BM25 + vector (LanceDB)          │
│  3. Retrieve top-12 chunks + source URLs            │
└───────────────────┬─────────────────────────────────┘
                    │ chunks + metadata
┌───────────────────▼─────────────────────────────────┐
│                  LLM Layer                          │
│  Prompt = system message + history + chunks + query │
│  Backend A: Ollama (local, NDJSON streaming)        │
│  Backend B: GitHub Models API (SSE streaming)       │
└─────────────────────────────────────────────────────┘
```

---

## Components

### 1. Graphical Interface (`src/gui.rs`)

- Built with GTK4 and libadwaita
- Product selector dropdown: Desktop, Server, Core, WSL, Flavors — each injects a product-specific system prompt addition
- Chat view with user and assistant message bubbles
- Streams LLM tokens into the view as they arrive via an async channel
- Markdown responses rendered as Pango markup (`src/markdown.rs`)

### 2. Terminal Interface (`src/cli.rs`)

- Interactive line-by-line chat loop reading from stdin
- Shows a spinner until the first token arrives, then streams tokens to stdout
- Exit on EOF (Ctrl-D) or by typing `exit`

### 3. RAG Pipeline (`src/vectordb.rs`)

- **Index**: LanceDB table (`docs`) with `source` (URL string) and `text` (chunk string) columns, plus a vector column for BGE-small-en-v1.5 embeddings
- **Retrieval**: hybrid search combining vector similarity and BM25 full-text search via LanceDB's `full_text_search()` + `nearest_to()` API; returns top 12 chunks
- **Embedding model**: `fastembed` with `BGESmallENV15` (384 dimensions); initialised at startup
- **Index path**: resolved at runtime — see index path resolution below

#### Index path resolution

The app resolves the LanceDB index in this order:

1. `UBUNTU_HELP_INDEX_PATH` env var — explicit override
2. `$SNAP_USER_DATA/index.lance` — written by the snap install/post-refresh hook
3. `$SNAP/index.lance` — read-only asset baked into the snap at build time
4. `target/index.lance` — dev build fallback

#### Follow-up question handling

When the user asks a follow-up question, the previous assistant reply is prepended to the RAG search query before embedding. This ensures that an ambiguous query like "how do I install it?" retrieves the right documentation topic.

### 4. LLM Interface (`src/llm.rs`)

The user selects a backend via CLI flag:

- **`--copilot`** (default off): uses the GitHub Models API (`https://models.github.ai/inference/chat/completions`) with SSE streaming. Default model: `gpt-4o-mini`. Override with `--model`.
  - **Note:** `claude-haiku-4.5` appears as enabled in the models list but returns HTTP 403 for streaming on individual Copilot plans. Use `claude-sonnet-4.5` or a GPT model if Claude streaming is needed.
- **Ollama** (default when `--copilot` is not set): connects to a local Ollama server via NDJSON streaming. Default model: `deepseek-r1:1.5b`. Default URL: `http://localhost:11434`. Override with `--model` and `--ollama-url`.

Both backends implement `chat()` (streams to stdout) and `chat_streaming()` (calls a token callback), dispatched through the `LlmClient` enum.

#### GitHub token resolution (Copilot mode)

The app resolves the GitHub OAuth token in this order:

1. `COPILOT_TOKEN` env var — use directly
2. GNOME Keyring / KWallet via Secret Service (`service=gh:github.com`) — works inside snaps with the `password-manager-service` plug
3. `gh auth token` CLI — last resort

### 5. Conversation History (`src/conversation.rs`)

- Stores bare user queries and assistant replies only — no RAG chunks, no system message
- Assembles the full message list for each LLM call: `[system, ...history, augmented_user_message]`
- The augmented user message includes the retrieved doc chunks prepended as context; it is never stored in history, so history stays lean regardless of conversation length

### 6. Product Prompts (`src/prompts.rs`, `product-prompts.toml`)

`product-prompts.toml` maps product names (Desktop, Server, Core, WSL, Flavors) to a system-prompt addition and a documentation URL prefix. The file is embedded at compile time via `include_str!`. The GUI injects the selected product's prompt addition into the system message for each LLM call.

---

## CLI

```
ask-ubuntu-docs [--model <name>] [--copilot] <subcommand>

Subcommands:
  chat   Interactive terminal chat session
  gui    Launch the GTK4 graphical interface

Global flags:
  --model <name>     Model name (env: MODEL)
  --copilot          Use GitHub Models API instead of Ollama

Subcommand flags (chat and gui):
  --ollama-url <url> Ollama server URL (env: OLLAMA_URL, default: http://localhost:11434)
```

---

## Key Dependencies

| Purpose | Crate |
|---|---|
| CLI argument parsing | `clap` (with `derive` and `env` features) |
| GTK4 GUI | `gtk4`, `libadwaita` |
| Markdown → Pango markup | `pulldown-cmark` |
| Embeddings | `fastembed` (BGE-small-en-v1.5) |
| Vector + hybrid search | `lancedb` |
| Arrow data access | `arrow-array` |
| HTTP (LLM API calls) | `reqwest` (with `json` and `stream` features) |
| JSON serialisation | `serde`, `serde_json` |
| TOML parsing (product prompts) | `toml` |
| Async runtime | `tokio` |
| Secret Service (keyring) | `secret-service` |
| Progress spinner | `indicatif` |

---

## Snap Packaging

- **Base**: `core26`
- **Confinement**: strict
- **License**: GPL-3.0+
- The `rag-index` snap part downloads the pre-built LanceDB index from the latest [ubuntu-docs-indexer](https://github.com/msuchane/ubuntu-docs-indexer) release and stages it at `$SNAP/index.lance`.
- The `install` and `post-refresh` hooks copy the index to `$SNAP_COMMON/index.lance` so LanceDB can open it with write access for manifest tracking.
- Key plugs: `network`, `home`, `password-manager-service`, `desktop`, `wayland`, `x11`, `opengl`.

---

## Constraints

- Must work on laptops without a dedicated GPU (local models via Ollama; small models preferred)
- All writable state goes to user-local directories (`$SNAP_USER_DATA`, `$SNAP_COMMON`)
- No user-side indexing — the index is pre-built and shipped with the snap

---

## Non-Goals

- GNOME Shell search provider integration (stretch goal)
- Agentic actions (detecting user intent and performing system actions on their behalf — stretch goal)
- In-app API token management UI (stretch goal)
- Support for non-Ubuntu/non-GNOME desktops
- General web search or crawling beyond official Ubuntu docs
- Fine-tuning or training models
- Multi-user or networked deployments

---

## Stretch Goal: GNOME Shell Search Provider

Implement `org.gnome.Shell.SearchProvider2` D-Bus interface so the app responds to queries typed in the GNOME overview. Triggered by a `??` or `ask:` prefix. Registered via a `.service` file for D-Bus autostart.

Requires `zbus` crate and a snap `dbus` or `unity7` plug for D-Bus access.

---

## Stretch Goal: Agentic Layer

Detect actionable intents in the user query (e.g. "change my wallpaper"), present a proposed action for user confirmation, and execute it via `gsettings`, `gio`, D-Bus calls, or shell commands.

---

## Stretch Goal: Rich Code Block Rendering

Replace the single `gtk::Label` per assistant response with a composite widget that renders fenced code blocks using GtkSourceView (`sourceview5` crate) with syntax highlighting and a "Copy" button. The `pulldown-cmark` event stream already distinguishes `Tag::CodeBlock(CodeBlockKind::Fenced(lang))` from inline code.

Requires `sourceview5` crate and `libgtksourceview-5-0` snap stage-package.

---

## Stretch Goal: API Token Settings Dialog

Add an `Adw.PreferencesDialog` accessible from the header bar where users can paste and save API tokens for GitHub Copilot and any future providers (e.g. OpenAI-compatible endpoints). This removes the need for any command-line setup for GUI users.

- Each provider gets its own password-entry row; tokens are stored in the system keyring via the `secret-service` crate with a provider-specific `service` attribute (e.g. `ask-ubuntu-docs:copilot`, `ask-ubuntu-docs:openai`).
- For GitHub Copilot, guide users to create a fine-grained PAT at `https://github.com/settings/personal-access-tokens` with no extra scopes — sufficient for the GitHub Models API.
- The existing `gh` keyring fallback (`service=gh:github.com`) is preserved for developers who already have `gh` authenticated.
- Uses the existing `password-manager-service` snap plug (no new permissions required).

---

## Stretch Goal: Index as a Content Snap

Publish the LanceDB index as a separate content snap (`ubuntu-help-index`) rebuilt by CI whenever any upstream documentation repository receives a commit. The main snap connects via the `content` interface, decoupling documentation update cadence from app release cadence.

When to implement: when index update latency becomes user-visible, or when the index grows large enough to make main snap refresh times unacceptable (roughly above 100 MB).
