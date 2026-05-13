# spec.md — AI-Based Desktop Help

## Purpose

Create an AI-powered assistant that answers questions about using Ubuntu. The user asks a natural-language question; the app retrieves relevant chunks from the local markdown-based Ubuntu documentation, feeds them to an LLM, and returns an answer plus links to relevant doc pages.

This is a local-first, context-based, offline tool. It uses official Ubuntu docs as its knowledge base and integrates optionally into GNOME Shell's overview search.

The minimum viable product is a CLI. A stretch goal is an integration with GNOME shell overview search.

---

## Scope

### In scope

- Cloning and keeping Ubuntu docs up-to-date locally
- Chunking and embedding markdown documentation files into a local vector database
- Answering user questions via a CLI interface
- Returning a generated answer, a link to the relevant local doc page, and optionally a clarifying question
- Ubuntu version awareness (e.g. a user on 22.04 gets answers from 22.04 docs)
- GNOME Shell search provider integration (stretch goal)
- Agentic actions offering to perform the described task on the user's behalf (stretch goal)

### Out of scope

- Cloud-based LLM backends are supported as a user-selectable option alongside local models (see LLM Interface)
- Support for non-Ubuntu/non-GNOME desktops
- General web search or crawling beyond official Ubuntu docs
- Fine-tuning or training models

## Design Principle: Compile-Time Indexing

Documentation must be cloned, chunked, embedded, and baked into the binary **at compile time** (`cargo build`), not when the user launches the app. This is a hard requirement, not a preference.

The rationale:
- Indexing the current corpus takes ~10 minutes and ~10 GB of RAM. Imposing this on users at first launch is unacceptable for a help tool.
- A snap must be immediately usable after installation with no setup step.
- The vector index is embedded directly into the binary via `include_bytes!`, making the app a fully self-contained executable with no external files or servers required at runtime.

Any architectural change (e.g. switching vector backends, adding more documentation sets) must preserve this property. If a future approach cannot perform indexing at compile time, it must be clearly marked as a stretch goal with an explicit plan for how to avoid first-run cost (e.g. CI-published pre-built indexes shipped as snap assets).

---



```
┌─────────────────────────────────────────────────────┐
│                   User Interface                    │
│         CLI (clap)  /  GNOME Shell Search           │
└───────────────────┬─────────────────────────────────┘
                    │ query
┌───────────────────▼─────────────────────────────────┐
│                  Query Pipeline                     │
│  1. Embed query (fastembed)                         │
│  2. Search vector DB (LanceDB)                      │
│  3. Retrieve top-k chunks + source paths            │
└───────────────────┬─────────────────────────────────┘
                    │ chunks + metadata
┌───────────────────▼─────────────────────────────────┐
│                  LLM Layer                          │
│  Prompt = system instructions + chunks + query      │
│  LLM generates: answer + clarifying question        │
│  Returns: answer, doc links, optional question      │
└───────────────────┬─────────────────────────────────┘
                    │ structured response
┌───────────────────▼─────────────────────────────────┐
│               Agentic Layer (stretch)               │
│  Parses intent → proposes action → awaits consent   │
│  Executes via D-Bus / gsettings / shell             │
└─────────────────────────────────────────────────────┘
```

---

## Components

### 1. Doc Syncer

- Clones several Ubuntu documentation Git repositories locally
- Detects Ubuntu version on the host machine
- On startup (or on demand), pulls latest changes
- Stores docs under a local path using environment variables.

### 2. Indexer

- Runs at **compile time** (`cargo build`) via `build.rs` — never at application startup
- Clones documentation repositories into `docs/` using `git clone --depth 1`
- Walks `docs/` recursively for `.md` files; inlines MyST `{include}` snippets
- Parses markdown with `pulldown-cmark`, strips markup to plain text
- Chunks text using `text-splitter`
- Embeds each chunk with `fastembed` (BGE-small-en-v1.5, 384 dimensions)
- Converts file paths to published documentation URLs using each repo's `conf.py`
- Writes a binary vector index to `$OUT_DIR/index.bin`, which is embedded into the binary via `include_bytes!`

### 3. Query Engine

- Embeds the user's query with the same embedding model (fastembed, BGE-small-en-v1.5)
- Performs brute-force cosine similarity search over the in-memory index (exact, microseconds at current scale)
- Returns top-k chunks with their published documentation URLs

### 4. LLM Interface

- Constructs a prompt: system message + retrieved chunks + user query
- The user selects their preferred backend via a CLI flag; local and cloud options are both first-class
- Supported backends:
  - **Local — Ollama** (`--model <name>`): connects to a locally running Ollama instance; default backend; no account or internet access required
  - **Cloud — GitHub Copilot / GitHub Models** (`--copilot`): uses the GitHub Models API; requires a GitHub account with Copilot access; model is selected by GitHub
  - **Cloud — OpenAI-compatible API** (future): any provider exposing an OpenAI-compatible `/v1/chat/completions` endpoint (OpenAI, Anthropic via proxy, Mistral, etc.) configurable via `--api-url` and `--api-key`
- The RAG pipeline is identical regardless of backend — only the final LLM call differs

### 5. CLI Frontend

- Built with `clap`
- Commands:
  - `desktop-help ask "<question>"` — single-shot query
  - `desktop-help chat` — interactive session
  - `desktop-help index` — manually trigger re-indexing
  - `desktop-help update` — pull latest docs
- Renders answer in terminal, prints doc links as clickable file:// URIs

### 6. GNOME Shell Search Provider (stretch)

- Implements `org.gnome.Shell.SearchProvider2` D-Bus interface
- Triggered by `??` or `ask:` prefix in GNOME overview search
- Registered via a `.service` file for D-Bus autostart
- Returns results as GNOME search result items; clicking opens the doc or a response window

### 7. Agentic Layer (stretch)

- Detects actionable intents in the user query (e.g. "change my wallpaper")
- Presents proposed action to user and requests confirmation
- Executes via `gsettings`, `gio`, D-Bus calls, or shell commands
- Logs actions taken

---

## Key Dependencies

| Purpose | Crate / Tool |
|---|---|
| CLI | `clap` |
| Markdown parsing | `pulldown-cmark` |
| Text chunking | `text-splitter` + `tokenizers` |
| Embeddings | `fastembed` |
| Vector search | brute-force cosine (in-memory, baked into binary) |
| LLM (option A) | `llama-cpp-rs` |
| LLM (option B) | Ollama HTTP API (`reqwest`) |
| LLM (option C) | Canonical inference snap HTTP API |
| GUI / GNOME bindings | `gtk-rs` (if GUI needed) |
| D-Bus | `zbus` |

---

## Data Model

### Vector DB record

```
{
  id: uuid,
  ubuntu_version: "24.04",
  source_file: "how-to/change-wallpaper.md",
  chunk_index: 3,
  chunk_text: "...",
  vector: [f32; N],
  content_hash: "sha256:...",
}
```

### LLM response

```
{
  answer: String,
  doc_links: Vec<(title: String, path: String)>,
  clarifying_question: Option<String>,
}
```

---

## Ubuntu Version Awareness

- At startup, read `/etc/os-release` to determine the current Ubuntu version
- Tag all indexed chunks with the detected version
- Filter vector search by version tag
- Future: allow the user to override the version (e.g. for testing or cross-version queries)

---

## Non-Goals (for now)

- A full GUI app (CLI first; GTK GUI is a future iteration)
- Multi-user or networked deployments
- Fine-tuning, RLHF, or feedback collection pipelines (noted as a future idea)
- Support for languages other than English

---

## Constraints

- Must work on laptops without a dedicated GPU
- Prefer small models (e.g. TinyLlama, Phi-3-mini) for performance
- All model downloads and DB writes go to user-local directories (no root required)
- Snap packaging must be feasible (no hard dependencies on system Python or system libs)

---

## Stretch Goal: LanceDB Vector Database

### Context

The current implementation uses brute-force cosine similarity over a flat vector index baked into the binary at build time. This is exact, fast enough for the current corpus (~8k–25k vectors), and requires no external dependencies at runtime. However it has no filtering capability: every query scores every chunk regardless of which documentation set or Ubuntu version it came from.

### Potential benefits

- **Per-product filtering** — with multiple documentation sets indexed (Desktop, Server, Core, etc.), queries can be scoped to only the relevant product. For example, a question about snaps would query Core docs; a question about GNOME settings would query Desktop docs. This avoids irrelevant chunks polluting the context window.
- **Per-version filtering** — chunks can be tagged with the Ubuntu version they describe (22.04, 24.04, 25.10…). At runtime the host version is read from `/etc/os-release` and queries are filtered to matching chunks, preventing outdated instructions from appearing in answers.
- **Incremental re-indexing** — individual files can be added, updated, or removed without rebuilding the entire index. Useful if doc updates are pulled at runtime rather than at build time.
- **ANN indexing** — LanceDB supports IVF and HNSW approximate nearest-neighbour indexes. Not beneficial at current scale, but becomes relevant above ~100k vectors.

### Why it is not the current approach

- **First-run indexing is unacceptable for this use case.** If the index lives in `~/.local/share/`, it must be built on the user's machine. At current corpus size this takes ~10 minutes and ~10 GB of RAM — a completely unacceptable first-run experience for a help tool.
- **External storage.** The index would live outside the binary, requiring a declared data directory in the snap (`$SNAP_USER_DATA`) and making the snap no longer fully self-contained.
- **Larger binary and longer builds.** LanceDB depends on Apache Arrow and DataFusion, adding ~20–40 MB to the binary and several minutes to cold compile times.
- **No net gain at current scale.** Brute-force cosine over ~25k vectors takes microseconds; ANN indexes provide no perceptible speedup.

### When it would become worthwhile

If the indexing pipeline is ever moved to a **separate offline build step** (e.g. a CI job that publishes a pre-built `index.lance` as a snap asset or OCI layer), then the first-run cost is eliminated and the filtering benefits become available without user-visible impact. Until that infrastructure exists, the embedded binary index is strictly preferable.

---



### Context

The current implementation embeds the vector index directly into the binary at build time using `include_bytes!`. This keeps deployment simple (single self-contained binary) and works well up to ~3 documentation repositories (~80 MB index, ~82 MB binary).

As more documentation sources are added, binary size and build-time RAM grow linearly: roughly 15 MB of index and 10 GB of peak build RAM per repository of similar size. Beyond ~5 repositories the embedded approach becomes impractical.

### Proposed approach

Move the vector index out of the binary and into a separate `index.bin` file shipped alongside it (e.g. at `/snap/ubuntu-desktop-help/current/share/index.bin`). At runtime, map the file into the process address space using `memmap2` instead of loading it into heap-allocated RAM.

**Key properties of the mmap approach:**

- **Fixed binary size** (~37 MB regardless of how many repositories are indexed)
- **Lazy page loading** — the OS only loads vector pages that are actually touched during a search query; for brute-force top-k search over a large index the working set stays in the hundreds of MB rather than the full index size
- **No change to build time** — the index is still generated by `build.rs` using the same pipeline; it is just staged as a separate file in the snap rather than embedded
- **Same index format** — the binary format already written by `build.rs` is suitable for mmap access without modification; vectors are stored as contiguous `f32` arrays, which can be sliced directly from the mapped memory without deserialisation

**Implementation sketch:**

```rust
// At startup
let file = File::open(index_path)?;
let mmap = unsafe { MmapOptions::new().map(&file)? };
// Parse header (dim, n_chunks) from mmap[0..16]
// Hold slices into the mmap for each vector — no heap allocation per vector
```

**Snap packaging change:** The index file must be generated during `snapcraft` build and staged into the snap prime directory. The snap `layout` stanza or a hardcoded read path under `$SNAP` handles the runtime lookup.

### When to implement

This becomes worthwhile when the index exceeds ~150 MB (roughly 5 repositories of current size) or when the binary size limit of the Snap Store becomes a concern.

---

## Stretch Goal: Rich Code Block Rendering in GUI

### Context

The current GUI renders assistant Markdown responses by converting them to Pango markup (`pulldown-cmark` → `gtk::Label::set_markup()`). This handles bold, italic, inline code, headings, lists, and links well. However, fenced code blocks are rendered as plain monospace text inside `<tt>` tags — no syntax highlighting, no visual separation from prose, and no copy button.

### Proposed approach

Replace the single `gtk::Label` per assistant response with a composite `gtk::Box` containing alternating widgets:

- **Prose segments** — `gtk::Label` with Pango markup, as today
- **Code block segments** — a dedicated widget built from:
  - `gtk::Frame` or a CSS-styled `gtk::Box` with a distinct background
  - `sourceview5::View` (GtkSourceView) with syntax highlighting language auto-detected from the fenced code block info string (e.g. `rust`, `bash`, `yaml`)
  - A "Copy" button in the top-right corner (`gtk::Button` overlaid via `gtk::Overlay`)

The `pulldown-cmark` event stream already distinguishes `Tag::CodeBlock(CodeBlockKind::Fenced(lang))` from inline code, so the split point is clean.

### Dependencies

- `sourceview5` crate + `libgtksourceview-5-dev` / `libgtksourceview-5-0` system packages
- Additional snap stage-package: `libgtksourceview-5-0`

### When to implement

When the LLM backend is wired up and real responses are flowing through the GUI, making code block quality visible and worth the added complexity.
