use std::collections::HashMap;
use std::env;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::process::Command;

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use serde::Deserialize;
use text_splitter::TextSplitter;

// Maximum characters per chunk; keeps each chunk within a useful slice of LLM context
const CHUNK_SIZE: usize = 512;
// Output dimension of BGE-small-en-v1.5; written into the index header so the runtime can verify
const EMBEDDING_DIM: usize = 384;

#[derive(Deserialize)]
struct DocsConfig {
    repo: Vec<RepoEntry>,
}

#[derive(Deserialize)]
struct RepoEntry {
    url: String,
    name: String,
}

/// Load the list of documentation repositories from `docs.toml` at build time.
fn load_docs_config() -> anyhow::Result<Vec<(String, String)>> {
    let src = fs::read_to_string("docs.toml")
        .map_err(|e| anyhow::anyhow!("failed to read docs.toml: {e}"))?;
    let config: DocsConfig = toml::from_str(&src)
        .map_err(|e| anyhow::anyhow!("docs.toml is malformed: {e}"))?;
    Ok(config.repo.into_iter().map(|r| (r.url, r.name)).collect())
}

fn main() -> anyhow::Result<()> {
    // Re-run this build script when build.rs or docs.toml changes.
    // We intentionally do NOT watch docs/ here: since we clone into docs/ ourselves,
    // watching it would cause an infinite rebuild loop.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=docs.toml");

    let out_dir = env::var("OUT_DIR")?;
    let index_path = Path::new(&out_dir).join("index.bin");

    // In debug builds, skip cloning and embedding entirely — write an empty index
    // so the app compiles and runs (with no RAG context). Use `cargo build --release`
    // for a fully functional binary.
    let profile = env::var("PROFILE").unwrap_or_default();
    if profile == "debug" {
        println!("cargo:warning=Debug build: skipping doc cloning and vectorisation (empty RAG index).");
        write_index(&index_path, EMBEDDING_DIM, &[], &[])?;
        return Ok(());
    }

    let repos = load_docs_config()?;
    clone_or_update_repos("docs", &repos)?;

    let chunks = load_chunks("docs");

    if chunks.is_empty() {
        println!("cargo:warning=No markdown files found in docs/; RAG index will be empty.");
        write_index(&index_path, EMBEDDING_DIM, &[], &[])?;
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

    write_index(&index_path, EMBEDDING_DIM, &chunks, &embeddings)?;

    println!(
        "cargo:warning=RAG index ready: {} vectors ({} dims).",
        chunks.len(),
        EMBEDDING_DIM
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

// Strips markdown syntax to plain text using pulldown-cmark's event stream.
fn markdown_to_plain_text(markdown: &str) -> String {
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    let parser = Parser::new_ext(markdown, opts);
    let mut text = String::new();
    for event in parser {
        match event {
            Event::Text(t) | Event::Code(t) => text.push_str(&t),
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

// Binary index format written to $OUT_DIR/index.bin and embedded by include_bytes! at runtime:
//   dim       u64 le   — embedding dimension (384 for BGE-small)
//   n_chunks  u64 le   — number of entries
//   per entry:
//     src_len u64 le + src_bytes   — source file path
//     txt_len u64 le + txt_bytes   — chunk plain text
//     dim × f32 le                 — embedding vector
fn write_index(
    path: &Path,
    dim: usize,
    chunks: &[Chunk],
    embeddings: &[Vec<f32>],
) -> anyhow::Result<()> {
    let mut f = File::create(path)?;
    f.write_all(&(dim as u64).to_le_bytes())?;
    f.write_all(&(chunks.len() as u64).to_le_bytes())?;
    for (chunk, vec) in chunks.iter().zip(embeddings.iter()) {
        let src = chunk.source.as_bytes();
        f.write_all(&(src.len() as u64).to_le_bytes())?;
        f.write_all(src)?;
        let txt = chunk.text.as_bytes();
        f.write_all(&(txt.len() as u64).to_le_bytes())?;
        f.write_all(txt)?;
        for val in vec {
            f.write_all(&val.to_le_bytes())?;
        }
    }
    Ok(())
}
