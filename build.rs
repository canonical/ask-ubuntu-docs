use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use arrow_array::{types::Float32Type, FixedSizeListArray, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use lancedb::index::{scalar::FtsIndexBuilder, Index};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use serde::Deserialize;
use text_splitter::TextSplitter;

// Maximum characters per chunk; keeps each chunk within a useful slice of LLM context
const CHUNK_SIZE: usize = 512;

#[derive(Deserialize)]
struct DocsConfig {
    repos: Vec<String>,
}

/// Load the list of documentation repository URLs from `docs.toml` at build time.
/// The clone directory name is inferred from the last segment of the URL.
fn load_docs_config() -> anyhow::Result<Vec<(String, String)>> {
    let src = fs::read_to_string("docs.toml")
        .map_err(|e| anyhow::anyhow!("failed to read docs.toml: {e}"))?;
    let config: DocsConfig = toml::from_str(&src)
        .map_err(|e| anyhow::anyhow!("docs.toml is malformed: {e}"))?;
    config
        .repos
        .into_iter()
        .map(|url| {
            let name = url
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow::anyhow!("cannot infer repo name from URL: {url}"))?
                .to_string();
            Ok((url, name))
        })
        .collect()
}

fn main() -> anyhow::Result<()> {
    tokio::runtime::Runtime::new()?.block_on(run())
}

async fn run() -> anyhow::Result<()> {
    // Re-run this build script when build.rs or docs.toml changes.
    // We intentionally do NOT watch docs/ here: since we clone into docs/ ourselves,
    // watching it would cause an infinite rebuild loop.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=docs.toml");

    let out_dir = env::var("OUT_DIR")?;
    let lancedb_path = Path::new(&out_dir).join("index.lance");

    // In debug builds, skip cloning and embedding entirely — write an empty index
    // so the app compiles and runs (with no RAG context). Use `cargo build --release`
    // for a fully functional binary.
    let profile = env::var("PROFILE").unwrap_or_default();
    if profile == "debug" {
        println!("cargo:warning=Debug build: skipping doc cloning and vectorisation (empty RAG index).");
        create_lancedb_index(&lancedb_path, &[], &[]).await?;
        return Ok(());
    }

    let repos = load_docs_config()?;
    clone_or_update_repos("docs", &repos)?;

    let chunks = load_chunks("docs");

    if chunks.is_empty() {
        println!("cargo:warning=No markdown files found in docs/; RAG index will be empty.");
        create_lancedb_index(&lancedb_path, &[], &[]).await?;
        return Ok(());
    }

    println!(
        "cargo:warning=Building RAG index from {} chunks (BGE-small model downloads ~130 MB on first run)…",
        chunks.len()
    );

    let mut embedder = TextEmbedding::try_new(
        InitOptions::new(EmbeddingModel::BGESmallENV15).with_show_download_progress(true),
    )?;

    let texts: Vec<String> = chunks.iter().map(|c| c.text.clone()).collect();
    let embeddings = embedder.embed(texts, None)?;

    create_lancedb_index(&lancedb_path, &chunks, &embeddings).await?;

    println!(
        "cargo:warning=RAG index ready: {} vectors (384 dims).",
        chunks.len()
    );

    Ok(())
}

// Ensures every repo in `repos` is present under `docs_dir`.
// Clones with --depth 1 on first run; does `git pull --ff-only` on subsequent runs.
fn clone_or_update_repos(docs_dir: &str, repos: &[(String, String)]) -> anyhow::Result<()> {
    fs::create_dir_all(docs_dir)?;
    for (url, name) in repos {
        let dest = Path::new(docs_dir).join(name);
        if dest.join(".git").is_dir() {
            println!("cargo:warning=Updating {name}…");
            let status = Command::new("git")
                .args(["-C", dest.to_str().unwrap(), "pull", "--ff-only", "--quiet"])
                .status()?;
            if !status.success() {
                println!("cargo:warning=Warning: `git pull` failed for {name}; using existing checkout.");
            }
        } else {
            println!("cargo:warning=Cloning {url} into docs/{name}…");
            let status = Command::new("git")
                .args(["clone", "--depth", "1", "--quiet", url, dest.to_str().unwrap()])
                .status()?;
            if !status.success() {
                anyhow::bail!("Failed to clone {url}");
            }
            println!("cargo:warning=Cloned {name}.");
        }
    }
    Ok(())
}

// Reads ogp_site_url from each repo's docs/conf.py.
// Returns a map from repo directory name (e.g. "ubuntu-desktop-documentation") to base URL.
fn read_base_urls(docs_dir: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let root = Path::new(docs_dir);
    let entries = match fs::read_dir(root) {
        Ok(e) => e,
        Err(_) => return map,
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let repo_path = entry.path();
        if !repo_path.is_dir() { continue; }
        let repo_name = match repo_path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        // conf.py is conventionally at <repo>/docs/conf.py
        let conf_path = repo_path.join("docs").join("conf.py");
        let conf = match fs::read_to_string(&conf_path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        if let Some(url) = extract_ogp_site_url(&conf) {
            map.insert(repo_name, url);
        }
    }
    map
}

// Extracts the string value of `ogp_site_url = "..."` from a conf.py file.
fn extract_ogp_site_url(conf: &str) -> Option<String> {
    for line in conf.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("ogp_site_url") {
            let rest = rest.trim().strip_prefix('=')?.trim();
            // Strip surrounding quotes (single or double)
            let url = rest
                .strip_prefix('"').and_then(|s| s.strip_suffix('"'))
                .or_else(|| rest.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
                .unwrap_or(rest);
            if !url.is_empty() {
                return Some(url.to_string());
            }
        }
    }
    None
}

// Converts a local file path to a published documentation URL using the rules:
//   1. Strip leading docs/ (local root directory)
//   2. Extract repo name and look up its base URL
//   3. If the next path component is `docs`, strip it too
//   4. Strip the trailing .md extension
//   5. Append a trailing slash
//
// Returns None if the path doesn't match any known repo base URL.
fn file_path_to_url(path: &Path, base_urls: &HashMap<String, String>) -> Option<String> {
    let mut components = path.components().peekable();

    // 1. Drop leading `docs` component
    let first = components.next()?.as_os_str().to_str()?;
    if first != "docs" { return None; }

    // 2. Repo name → base URL
    let repo_name = components.next()?.as_os_str().to_str()?;
    let base_url = base_urls.get(repo_name)?.trim_end_matches('/');

    // 3. If next component is `docs`, drop it
    if components.peek().and_then(|c| c.as_os_str().to_str()) == Some("docs") {
        components.next();
    }

    // 4. Collect remaining components; strip .md from the last one
    let mut parts: Vec<String> = components
        .map(|c| c.as_os_str().to_str().unwrap_or("").to_string())
        .collect();
    if let Some(last) = parts.last_mut() {
        if let Some(stem) = last.strip_suffix(".md") {
            *last = stem.to_string();
        }
    }

    // 5. Join and add trailing slash
    let path_str = parts.join("/");
    Some(format!("{base_url}/{path_str}/"))
}

struct Chunk {
    source: String,
    text: String,
}

// Walks `dir` recursively for .md files, strips markdown to plain text, and splits into chunks.
fn load_chunks(dir: &str) -> Vec<Chunk> {
    let base_urls = read_base_urls(dir);

    let mut md_files = Vec::new();
    collect_md_files(Path::new(dir), &mut md_files);
    // Sort for a deterministic index regardless of filesystem ordering
    md_files.sort();

    let splitter = TextSplitter::new(CHUNK_SIZE);
    let mut chunks = Vec::new();

    for file_path in &md_files {
        let raw = match fs::read_to_string(file_path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        // Inline any MyST {include} directives before parsing so that
        // reused content is present in the plain-text output
        let expanded = expand_includes(&raw, file_path);
        let plain = markdown_to_plain_text(&expanded);
        let source = file_path_to_url(file_path, &base_urls)
            .unwrap_or_else(|| file_path.display().to_string());
        for chunk_text in splitter.chunks(&plain) {
            let text = chunk_text.trim().to_string();
            if !text.is_empty() {
                chunks.push(Chunk { source: source.clone(), text });
            }
        }
    }

    chunks
}

// Recursively collects all .md file paths under `dir` into `out`.
fn collect_md_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(iter) => iter,
        Err(_) => return,
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            collect_md_files(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("md") {
            out.push(path);
        }
    }
}

// Expands MyST `{include}` directives in `content` by inlining the referenced files.
// Recursively resolves nested includes (included files may themselves include others).
//
// Supported fence styles:
//   ```{include} path/to/file.txt
//   ```
//   ::::{include} path/to/file.txt
//   ::::
//
// Paths may be relative to the current file or absolute within the repo
// (starting with `/`, resolved from the nearest ancestor `docs/` directory).
fn expand_includes(content: &str, file_path: &Path) -> String {
    // Resolve the docs root as the first ancestor directory named "docs"
    let docs_root: Option<std::path::PathBuf> = file_path
        .ancestors()
        .find(|a| a.file_name().and_then(|n| n.to_str()) == Some("docs"))
        .map(|p| p.to_path_buf());

    expand_includes_inner(content, file_path, &docs_root, 0)
}

// Inner recursive helper; `depth` guards against circular includes.
fn expand_includes_inner(
    content: &str,
    file_path: &Path,
    docs_root: &Option<std::path::PathBuf>,
    depth: usize,
) -> String {
    if depth > 8 {
        return content.to_string();
    }

    let file_dir = file_path.parent().unwrap_or(Path::new("."));
    let mut output = String::with_capacity(content.len());
    let mut lines = content.lines().peekable();

    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        if let Some(include_path) = parse_include_directive(trimmed) {
            // Consume lines up to and including the matching closing fence
            let fence_char = trimmed.chars().next().unwrap_or('`');
            let fence_len = trimmed.chars().take_while(|&c| c == fence_char).count();
            let closing: String = std::iter::repeat(fence_char).take(fence_len).collect();
            for next in lines.by_ref() {
                if next.trim() == closing || next.trim().starts_with(&closing) {
                    break;
                }
            }

            // Resolve the included file path
            let resolved = if include_path.starts_with('/') {
                // Absolute path within the repo: resolve from docs root
                docs_root
                    .as_deref()
                    .map(|r| r.join(include_path.trim_start_matches('/')))
            } else {
                Some(file_dir.join(&include_path))
            };

            if let Some(inc_path) = resolved {
                match fs::read_to_string(&inc_path) {
                    Ok(inc) => {
                        // Recursively expand any includes inside the included file
                        let expanded = expand_includes_inner(&inc, &inc_path, docs_root, depth + 1);
                        output.push_str(&expanded);
                        if !expanded.ends_with('\n') {
                            output.push('\n');
                        }
                    }
                    Err(_) => {
                        // File not found — emit a note so the chunk at least mentions it
                        output.push_str(&format!("[included file: {include_path}]\n"));
                    }
                }
            }
        } else {
            output.push_str(line);
            output.push('\n');
        }
    }

    output
}

// Returns the include path if `line` is a MyST {include} directive opener, otherwise None.
fn parse_include_directive(line: &str) -> Option<String> {
    // Accept ``` or :::: fences of any length
    let rest = if line.starts_with("```") {
        line.trim_start_matches('`')
    } else if line.starts_with("::::") || line.starts_with(":::") {
        line.trim_start_matches(':')
    } else {
        return None;
    };
    // rest should now be `{include} path`
    let rest = rest.trim();
    let rest = rest.strip_prefix("{include}")?.trim();
    if rest.is_empty() {
        return None;
    }
    Some(rest.to_string())
}

// Strips YAML frontmatter (--- ... ---) and MyST anchor labels ((label)=) from a
// Markdown document before parsing, so they don't pollute the plain-text chunks.
fn strip_myst_noise(markdown: &str) -> &str {
    let mut s = markdown;

    // Strip YAML frontmatter: document starts with `---\n…\n---`
    if let Some(rest) = s.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---\n") {
            s = &rest[end + 5..]; // skip past the closing `---\n`
        } else if let Some(end) = rest.find("\n---") {
            // Allow `---` at the very end of file with no trailing newline
            let after = &rest[end + 4..];
            if after.trim().is_empty() {
                s = after;
            }
        }
    }

    s
}

// Strips markdown syntax to plain text using pulldown-cmark's event stream.
fn markdown_to_plain_text(markdown: &str) -> String {
    let markdown = strip_myst_noise(markdown);

    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    let parser = Parser::new_ext(markdown, opts);
    let mut text = String::new();
    for event in parser {
        match event {
            Event::Text(t) => {
                // Drop MyST anchor labels: lines of the form `(some-label)=`
                let t = t.trim();
                if t.starts_with('(') && t.ends_with(")=") {
                    continue;
                }
                text.push_str(t);
                text.push(' ');
            }
            Event::Code(t) => text.push_str(&t),
            Event::SoftBreak | Event::HardBreak => text.push(' '),
            Event::End(TagEnd::Paragraph)
            | Event::End(TagEnd::Heading { .. })
            | Event::End(TagEnd::Item)
            | Event::End(TagEnd::CodeBlock) => text.push('\n'),
            Event::Start(Tag::CodeBlock(_)) => text.push('\n'),
            _ => {}
        }
    }
    text
}

// Creates a LanceDB table at `path` with schema {source, text, vector}.
// For debug builds, pass empty slices to create a schema-only table (no rows, no FTS index).
// For release builds, populates the table with chunks + embeddings and builds an FTS index.
async fn create_lancedb_index(
    path: &Path,
    chunks: &[Chunk],
    embeddings: &[Vec<f32>],
) -> anyhow::Result<()> {
    // BGE-small-en-v1.5 output dimension
    const DIM: i32 = 384;

    if path.exists() {
        fs::remove_dir_all(path)?;
    }

    let schema = Arc::new(Schema::new(vec![
        Field::new("source", DataType::Utf8, false),
        Field::new("text", DataType::Utf8, false),
        Field::new(
            "vector",
            DataType::FixedSizeList(Arc::new(Field::new("item", DataType::Float32, true)), DIM),
            false,
        ),
    ]));

    let batch = if chunks.is_empty() {
        RecordBatch::new_empty(schema.clone())
    } else {
        let sources = Arc::new(StringArray::from(
            chunks.iter().map(|c| c.source.as_str()).collect::<Vec<_>>(),
        ));
        let texts = Arc::new(StringArray::from(
            chunks.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
        ));
        let vectors = Arc::new(FixedSizeListArray::from_iter_primitive::<Float32Type, _, _>(
            embeddings
                .iter()
                .map(|v| Some(v.iter().map(|&f| Some(f)))),
            DIM,
        ));
        RecordBatch::try_new(schema.clone(), vec![sources, texts, vectors])?
    };

    let path_str = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("LanceDB path is not valid UTF-8"))?;
    let db = lancedb::connect(path_str).execute().await?;
    let tbl = db
        .create_table("docs", vec![batch])
        .execute()
        .await?;

    // Only build the FTS index when there is actual content to index.
    if !chunks.is_empty() {
        tbl.create_index(&["text"], Index::FTS(FtsIndexBuilder::default()))
            .execute()
            .await?;
    }

    Ok(())
}
