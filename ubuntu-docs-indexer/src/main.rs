use flate2::write::GzEncoder;
use flate2::Compression;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use arrow_array::{types::Float32Type, FixedSizeListArray, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use clap::Parser;
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use indicatif::{ProgressBar, ProgressStyle};
use lancedb::index::{scalar::FtsIndexBuilder, Index};
use pulldown_cmark::{Event, Options, Parser as MdParser, Tag, TagEnd};
use serde::Deserialize;
use text_splitter::TextSplitter;

// Maximum characters per chunk; keeps each chunk within a useful slice of LLM context
const CHUNK_SIZE: usize = 512;

#[derive(Parser)]
#[command(
    name = "ubuntu-docs-indexer",
    about = "Build a LanceDB RAG index from Ubuntu documentation repositories",
    long_about = "Clones Ubuntu documentation repositories and generates a LanceDB vector index \
                  for use by ubuntu-desktop-help."
)]
struct Cli {
    /// Path to the docs configuration TOML file listing documentation repository URLs.
    #[arg(long, default_value = "docs.toml")]
    docs_config: PathBuf,

    /// Directory into which documentation repositories are cloned.
    #[arg(long, default_value = "docs")]
    docs_dir: PathBuf,

    /// Output path for the generated index.lance directory.
    #[arg(long, default_value = "target/index.lance")]
    output: PathBuf,
}

#[derive(Deserialize)]
struct DocsConfig {
    repos: Vec<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let repos = load_docs_config(&cli.docs_config)?;
    clone_or_update_repos(&cli.docs_dir, &repos)?;

    let chunks = load_chunks(&cli.docs_dir);

    if chunks.is_empty() {
        eprintln!("Warning: no markdown files found in {}; index will be empty.", cli.docs_dir.display());
        create_lancedb_index(&cli.output, &[], &[]).await?;
        return Ok(());
    }

    eprintln!(
        "Building RAG index from {} chunks (BGE-small model downloads ~130 MB on first run)…",
        chunks.len()
    );

    let pb = ProgressBar::new(chunks.len() as u64);
    pb.set_style(
        ProgressStyle::with_template("[{elapsed_precise}] [{bar:40}] {pos}/{len} chunks embedded")
            .unwrap()
            .progress_chars("=> "),
    );

    let texts: Vec<String> = chunks.iter().map(|c| c.text.clone()).collect();

    let embeddings = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<Vec<f32>>> {
        let mut embedder = TextEmbedding::try_new(
            InitOptions::new(EmbeddingModel::BGESmallENV15).with_show_download_progress(true),
        )?;
        let embeddings = embedder.embed(texts, None)?;
        Ok(embeddings)
    })
    .await
    .map_err(|e| anyhow::anyhow!("embedding task panicked: {e}"))??;

    pb.finish_with_message("embedding done");

    create_lancedb_index(&cli.output, &chunks, &embeddings).await?;

    eprintln!(
        "RAG index written to {}: {} vectors (384 dims).",
        cli.output.display(),
        chunks.len()
    );

    let tarball_path = {
        let mut p = cli.output.clone();
        p.set_extension("lance.tar.gz");
        p
    };
    compress_index(&cli.output, &tarball_path)?;
    eprintln!("Compressed index written to {}.", tarball_path.display());

    Ok(())
}

/// Load the list of documentation repository URLs from a TOML config file.
/// Returns a list of (url, repo-name) pairs.
fn load_docs_config(path: &Path) -> anyhow::Result<Vec<(String, String)>> {
    let src = fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.display()))?;
    let config: DocsConfig = toml::from_str(&src)
        .map_err(|e| anyhow::anyhow!("{} is malformed: {e}", path.display()))?;
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

/// Ensures every repo in `repos` is present under `docs_dir`.
/// Clones with --depth 1 on first run; does `git pull --ff-only` on subsequent runs.
fn clone_or_update_repos(docs_dir: &Path, repos: &[(String, String)]) -> anyhow::Result<()> {
    fs::create_dir_all(docs_dir)?;
    for (url, name) in repos {
        let dest = docs_dir.join(name);
        if dest.join(".git").is_dir() {
            eprintln!("Updating {name}…");
            let status = Command::new("git")
                .args(["-C", dest.to_str().unwrap(), "pull", "--ff-only", "--quiet"])
                .status()?;
            if !status.success() {
                eprintln!("Warning: `git pull` failed for {name}; using existing checkout.");
            }
        } else {
            eprintln!("Cloning {url} into {}…", dest.display());
            let status = Command::new("git")
                .args(["clone", "--depth", "1", "--quiet", url, dest.to_str().unwrap()])
                .status()?;
            if !status.success() {
                anyhow::bail!("failed to clone {url}");
            }
            eprintln!("Cloned {name}.");
        }
    }
    Ok(())
}

/// Reads `ogp_site_url` from each repo's `docs/conf.py`.
/// Returns a map from repo directory name to base URL.
fn read_base_urls(docs_dir: &Path) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let entries = match fs::read_dir(docs_dir) {
        Ok(e) => e,
        Err(_) => return map,
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let repo_path = entry.path();
        if !repo_path.is_dir() {
            continue;
        }
        let repo_name = match repo_path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
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

/// Extracts the string value of `ogp_site_url = "..."` from a conf.py file.
fn extract_ogp_site_url(conf: &str) -> Option<String> {
    for line in conf.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("ogp_site_url") {
            let rest = rest.trim().strip_prefix('=')?.trim();
            let url = rest
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .or_else(|| rest.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
                .unwrap_or(rest);
            if !url.is_empty() {
                return Some(url.to_string());
            }
        }
    }
    None
}

/// Converts a local file path to a published documentation URL.
fn file_path_to_url(path: &Path, docs_dir: &Path, base_urls: &HashMap<String, String>) -> Option<String> {
    // Strip the docs_dir prefix to get a path relative to it
    let rel = path.strip_prefix(docs_dir).ok()?;
    let mut components = rel.components().peekable();

    // First component is the repo name
    let repo_name = components.next()?.as_os_str().to_str()?;
    let base_url = base_urls.get(repo_name)?.trim_end_matches('/');

    // If next component is `docs`, drop it
    if components.peek().and_then(|c| c.as_os_str().to_str()) == Some("docs") {
        components.next();
    }

    // Collect remaining; strip .md from last
    let mut parts: Vec<String> = components
        .map(|c| c.as_os_str().to_str().unwrap_or("").to_string())
        .collect();
    if let Some(last) = parts.last_mut() {
        if let Some(stem) = last.strip_suffix(".md") {
            *last = stem.to_string();
        }
    }

    let path_str = parts.join("/");
    Some(format!("{base_url}/{path_str}/"))
}

struct Chunk {
    source: String,
    text: String,
}

/// Walks `docs_dir` recursively for .md files, strips markdown to plain text, and splits into chunks.
fn load_chunks(docs_dir: &Path) -> Vec<Chunk> {
    let base_urls = read_base_urls(docs_dir);

    // For each cloned repository under `docs_dir`, index only the `docs/` subdirectory
    // if one exists; otherwise fall back to the repository root.
    let mut md_files = Vec::new();
    if let Ok(entries) = fs::read_dir(docs_dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let repo_path = entry.path();
            if !repo_path.is_dir() {
                continue;
            }
            let docs_subdir = repo_path.join("docs");
            let walk_root = if docs_subdir.is_dir() { docs_subdir } else { repo_path };
            collect_md_files(&walk_root, &mut md_files);
        }
    }
    // Sort for a deterministic index regardless of filesystem ordering
    md_files.sort();

    let splitter = TextSplitter::new(CHUNK_SIZE);
    let mut chunks = Vec::new();

    for file_path in &md_files {
        let raw = match fs::read_to_string(file_path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let expanded = expand_includes(&raw, file_path);
        let plain = markdown_to_plain_text(&expanded);
        let source = file_path_to_url(file_path, docs_dir, &base_urls)
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

/// Recursively collects all .md file paths under `dir` into `out`.
fn collect_md_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(iter) => iter,
        Err(_) => return,
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            collect_md_files(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("md") {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !matches!(name, "README.md" | "CONTRIBUTING.md") {
                out.push(path);
            }
        }
    }
}

/// Expands MyST `{include}` directives in `content` by inlining the referenced files.
fn expand_includes(content: &str, file_path: &Path) -> String {
    let docs_root: Option<PathBuf> = file_path
        .ancestors()
        .find(|a| a.file_name().and_then(|n| n.to_str()) == Some("docs"))
        .map(|p| p.to_path_buf());
    expand_includes_inner(content, file_path, &docs_root, 0)
}

fn expand_includes_inner(
    content: &str,
    file_path: &Path,
    docs_root: &Option<PathBuf>,
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
            let fence_char = trimmed.chars().next().unwrap_or('`');
            let fence_len = trimmed.chars().take_while(|&c| c == fence_char).count();
            let closing: String = std::iter::repeat(fence_char).take(fence_len).collect();
            for next in lines.by_ref() {
                if next.trim() == closing || next.trim().starts_with(&closing) {
                    break;
                }
            }
            let resolved = if include_path.starts_with('/') {
                docs_root
                    .as_deref()
                    .map(|r| r.join(include_path.trim_start_matches('/')))
            } else {
                Some(file_dir.join(&include_path))
            };
            if let Some(inc_path) = resolved {
                match fs::read_to_string(&inc_path) {
                    Ok(inc) => {
                        let expanded = expand_includes_inner(&inc, &inc_path, docs_root, depth + 1);
                        output.push_str(&expanded);
                        if !expanded.ends_with('\n') {
                            output.push('\n');
                        }
                    }
                    Err(_) => {
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

fn parse_include_directive(line: &str) -> Option<String> {
    let rest = if line.starts_with("```") {
        line.trim_start_matches('`')
    } else if line.starts_with("::::") || line.starts_with(":::") {
        line.trim_start_matches(':')
    } else {
        return None;
    };
    let rest = rest.trim().strip_prefix("{include}")?.trim();
    if rest.is_empty() {
        return None;
    }
    Some(rest.to_string())
}

fn strip_myst_noise(markdown: &str) -> &str {
    let mut s = markdown;
    if let Some(rest) = s.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---\n") {
            s = &rest[end + 5..];
        } else if let Some(end) = rest.find("\n---") {
            let after = &rest[end + 4..];
            if after.trim().is_empty() {
                s = after;
            }
        }
    }
    s
}

fn markdown_to_plain_text(markdown: &str) -> String {
    let markdown = strip_myst_noise(markdown);
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    let parser = MdParser::new_ext(markdown, opts);
    let mut text = String::new();
    for event in parser {
        match event {
            Event::Text(t) => {
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

/// Compresses `index_dir` into a `.tar.gz` archive at `dest`.
fn compress_index(index_dir: &Path, dest: &Path) -> anyhow::Result<()> {
    let file = fs::File::create(dest)
        .map_err(|e| anyhow::anyhow!("failed to create {}: {e}", dest.display()))?;
    let gz = GzEncoder::new(file, Compression::best());
    let mut tar = tar::Builder::new(gz);
    // Store files with uid/gid 0 so the archive is portable across systems
    // with different user databases (e.g. snap build containers).
    tar.mode(tar::HeaderMode::Deterministic);
    // Archive the directory under its own name (e.g. "index.lance/...")
    let dir_name = index_dir
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("index path has no file name"))?;
    tar.append_dir_all(dir_name, index_dir)
        .map_err(|e| anyhow::anyhow!("failed to archive {}: {e}", index_dir.display()))?;
    tar.finish()?;
    Ok(())
}

/// Creates a LanceDB table at `path` with schema {source, text, vector}.
/// When `chunks` is empty, creates a schema-only table with no rows.
async fn create_lancedb_index(
    path: &Path,
    chunks: &[Chunk],
    embeddings: &[Vec<f32>],
) -> anyhow::Result<()> {
    const DIM: i32 = 384;

    if path.exists() {
        fs::remove_dir_all(path)?;
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
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
            embeddings.iter().map(|v| Some(v.iter().map(|&f| Some(f)))),
            DIM,
        ));
        RecordBatch::try_new(schema.clone(), vec![sources, texts, vectors])?
    };

    let path_str = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("LanceDB path is not valid UTF-8"))?;
    let db = lancedb::connect(path_str).execute().await?;
    let tbl = db.create_table("docs", vec![batch]).execute().await?;

    if !chunks.is_empty() {
        tbl.create_index(&["text"], Index::FTS(FtsIndexBuilder::default()))
            .execute()
            .await?;
    }

    Ok(())
}
