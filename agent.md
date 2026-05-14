# agent.md — Developer & Agent Guide

> This file tells human developers and AI coding agents **how** to work on this project.
> For **what** the project is and does, see [`spec.md`](./spec.md).
> Always keep `spec.md` in context when making architectural decisions.

---

## Language & Toolchain

- **Language**: Rust (stable, latest stable toolchain via `rustup`)
- **Minimum Rust edition**: 2021
- **Package manager**: Cargo
- **Formatter**: `rustfmt` (run before committing)
- **Linter**: `clippy` (zero warnings policy on new code)
- **Documentation**: Always comment your code, at least one comment per code block. Explain your reasoning shortly. Assume that the reader only has very basic Rust knowledge.

```bash
rustup update stable
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
```

---

## Repository Layout

```
desktop-help/
├── spec.md               # What the app is (keep in context)
├── agent.md              # This file
├── Cargo.toml            # Workspace root
├── Cargo.lock
├── crates/
│   ├── cli/              # CLI frontend (clap)
│   ├── indexer/          # Doc syncing, chunking, embedding, LanceDB writes
│   ├── query/            # Vector search + LLM prompt construction
│   ├── llm/              # LLM backend abstraction (ollama / llama-cpp / inference snap)
│   └── search-provider/  # GNOME Shell D-Bus search provider (stretch)
├── docs-cache/           # Gitignored; local clone of Ubuntu docs
├── tests/
│   ├── integration/
│   └── fixtures/         # Small sample .md files for tests
└── snap/
    └── snapcraft.yaml    # Snap packaging config
```

Use a Cargo workspace so crates can be developed and tested independently.

---

## Key Crates

Add these to the relevant `Cargo.toml` files:

```toml
# Parsing & chunking
pulldown-cmark = "0.11"
text-splitter = { version = "0.14", features = ["markdown", "tiktoken-rs"] }

# Embeddings
fastembed = "3"

# Vector DB
lancedb = "0.8"
arrow-array = "51"        # LanceDB uses Arrow under the hood

# LLM (Ollama path — start here)
reqwest = { version = "0.12", features = ["json"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"

# CLI
clap = { version = "4", features = ["derive"] }

# D-Bus (search provider stretch)
zbus = "4"

# Async runtime
tokio = { version = "1", features = ["full"] }

# Utilities
uuid = { version = "1", features = ["v4"] }
sha2 = "0.10"
anyhow = "1"
tracing = "0.1"
tracing-subscriber = "0.3"
```

---

## Build & Run

```bash
# Build everything
cargo build

# Run the CLI
cargo run -p cli -- ask "How do I change my wallpaper?"

# Run in interactive chat mode
cargo run -p cli -- chat

# Trigger manual re-index
cargo run -p cli -- index

# Pull latest docs
cargo run -p cli -- update
```

---

## Environment & Config

The app reads config from `~/.config/desktop-help/config.toml` (created with defaults on first run).

```toml
[docs]
repo_url = "https://github.com/canonical/ubuntu-desktop-documentation"
local_path = "~/.local/share/desktop-help/docs"

[embedding]
model = "BAAI/bge-small-en-v1.5"   # downloaded by fastembed on first run

[llm]
backend = "ollama"                  # "ollama" | "llama-cpp" | "inference-snap"
ollama_url = "http://localhost:11434"
model = "phi3:mini"

[search]
top_k = 5
```

The Ubuntu version is detected automatically from `/etc/os-release` and is not stored in config.

---

## Development Sequence

Work in this order. Don't move to the next step until the current one produces observable output.

1. **Indexer — basic pipeline**
   - Clone docs repo to `docs-cache/`
   - Walk `.md` files, parse with `pulldown-cmark`, extract plain text
   - Chunk with `text-splitter`
   - Embed with `fastembed`
   - Write to LanceDB table with schema from `spec.md`
   - Print chunk count and a sample vector to confirm it works

2. **Query engine — retrieval**
   - Embed a hardcoded test query
   - Search LanceDB, print top-5 chunks + source paths
   - Confirm semantic relevance by inspection

3. **LLM interface — Ollama**
   - Start with Ollama (easiest HTTP API, good for dev)
   - POST query + retrieved chunks to Ollama `/api/chat`
   - Parse and print the response
   - Confirm the answer references the retrieved content

4. **CLI — wire it together**
   - Implement `ask` subcommand end-to-end
   - Implement `chat` for multi-turn sessions (maintain message history in memory)
   - Add `index` and `update` subcommands

5. **Ubuntu version filtering**
   - Read `/etc/os-release`
   - Tag chunks at index time; filter at query time
   - Test with a fixture that has version-specific content

6. **GNOME Shell search provider** *(stretch)*
   - Implement `org.gnome.Shell.SearchProvider2` with `zbus`
   - Register `.service` file
   - Test `??` prefix trigger in GNOME overview

7. **Agentic layer** *(stretch)*
   - Detect actionable intents via LLM classification
   - Propose action to user (confirm/deny)
   - Execute via `gsettings` / D-Bus

---

## Testing

```bash
# Unit tests
cargo test --all

# Integration tests (require Ollama running locally)
DESKTOP_HELP_TEST_INTEGRATION=1 cargo test -p query --test integration
```

### Test fixtures

Put small, self-contained `.md` files in `tests/fixtures/`. Tests should not depend on the real docs repo being present.

### What to test

- Chunking produces non-empty, bounded chunks
- Embedding round-trips (embed → store → retrieve returns same chunk)
- Query returns results ranked by cosine similarity
- Version filter excludes chunks from wrong Ubuntu version
- LLM prompt is well-formed (test prompt construction without calling the LLM)
- CLI `ask` exits 0 on valid input, non-zero on missing config

---

## LLM Prompt Template

Keep prompt construction in one place (`crates/query/src/prompt.rs`).

```
System:
You are a helpful assistant for Ubuntu users.
Answer the user's question using only the documentation excerpts provided below.
If the answer is not in the excerpts, say so clearly.
If the question is ambiguous, ask one clarifying question.
Always cite the source document for any claim.

Documentation excerpts:
{chunks}

User question:
{query}
```

---

## Ollama Setup (local dev)

```bash
# Install Ollama
curl -fsSL https://ollama.com/install.sh | sh

# Pull a small model
ollama pull phi3:mini

# Confirm it's running
curl http://localhost:11434/api/tags
```

For CI or machines without Ollama, mock the LLM layer via a trait + test double.

---

## LLM Backend Abstraction

Define a trait in `crates/llm/src/lib.rs` so backends are swappable:

```rust
#[async_trait]
pub trait LlmBackend: Send + Sync {
    async fn complete(&self, prompt: &str) -> anyhow::Result<String>;
}
```

Implement `OllamaBackend`, `LlamaCppBackend`, and a `MockBackend` for tests. The CLI selects the backend from config.

---

## Snap Packaging Notes

- Target `core24` base
- Use `rustup` plugin or pre-built binary stage
- Bundle fastembed model in the snap (download at build time, include in `stage`)
- LLM options:
  - `llama-cpp` plugin: self-contained, no external services needed — **preferred for packaging**
  - Ollama as a content snap interface: cleaner model management, requires Ollama snap installed
- Store docs-cache and LanceDB in `$SNAP_USER_DATA`

---

## Code Style Notes for Agents

- Use `anyhow::Result` for error propagation throughout; avoid `.unwrap()` outside tests
- Use `tracing` for logging (`tracing::info!`, `tracing::debug!`); never `println!` in library crates
- Keep each crate focused on one responsibility (see repo layout above)
- Prefer iterators and `?`-propagation over explicit loops and match chains where idiomatic
- Do not add new dependencies without checking they compile in a no-std or snap-constrained context first
- Avoid generating large blocks of commented-out code
- When in doubt, refer back to `spec.md` before adding new behaviour

### GUI ↔ CLI feature parity

The GUI (`src/gui.rs`) and the CLI (`src/main.rs`) share the same backend logic (RAG, LLM, history). **Whenever you change the behaviour of one interface, apply the equivalent change to the other** — unless the change is genuinely interface-specific (e.g. a widget layout tweak, or a terminal escape code). The only thing that should differ between the two is how input is gathered and output is displayed.

Concretely:
- A bug fixed in `run_chat()` should also be fixed in the GUI's message-send handler, and vice versa.
- A new feature (e.g. RAG query contextualisation, history trimming, model selection) must land in both.
- If the change truly cannot be applied to the other interface, leave a `// TODO(parity):` comment explaining why.

---

## System Package Dependencies

This project requires certain system packages (e.g. `protobuf-compiler` for LanceDB's `lance-encoding` crate).
**Agents do not have `sudo` access.** If a build fails because a system package is missing, stop and ask the user to install it:

```
sudo apt-get install -y <package-name>
```

Known required packages:
- `protobuf-compiler` — required by `lance-encoding` (transitive LanceDB dependency)
