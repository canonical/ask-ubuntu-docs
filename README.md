# Desktop help app

[![Watch the demo](https://img.youtube.com/vi/zOLcazhCQBk/0.jpg)](https://www.youtube.com/watch?v=zOLcazhCQBk)

An experimental Rust application for answering Ubuntu questions with a local documentation context.

The current prototype is a CLI chat interface. It can talk to either:

- a local model served through Ollama
- a remote model via GitHub Copilot and the GitHub Models API

## Prerequisites

**Hardware:** At least 2 GB of RAM and ~2 GB of disk space (for `deepseek-r1:1.5b`).

**Rust and Cargo**

```bash
sudo snap install rustup --classic
rustup toolchain install stable
```

## Run the app with a local model

The app talks to a local model through Ollama. Install it first:

```bash
sudo snap install ollama
```

Pull a model, for example:

```bash
ollama pull deepseek-r1:1.5b
ollama pull tinyllama
```

Start the app, passing the model name you pulled:

```bash
cargo run chat --model tinyllama
```

If `--model` is not specified, the app defaults to `deepseek-r1:1.5b`.

You can also set the model and server URL via environment variables:

```bash
OLLAMA_MODEL=deepseek-r1:1.5b OLLAMA_URL=http://localhost:11434 cargo run chat
```

For a release build, add `--release`:

```bash
cargo run --release chat --model tinyllama
```

## Run the app with GitHub Copilot

Use this mode to switch from a local Ollama-served model to a remote model via the GitHub Models API.

This can feel faster than running a local model because the inference work does
not happen on your machine, though the result still depends on network latency.


First, authenticate with GitHub Copilot. The simplest way is via the GitHub CLI:

```bash
sudo apt install gh
gh auth login
gh auth token  # verify it works
```

Alternatively, set `COPILOT_TOKEN` directly — this takes precedence over the GitHub CLI. Generate a token at [github.com/settings/tokens](https://github.com/settings/tokens) with the `models:read` scope, or use a token from an existing GitHub Copilot subscription.

In Bash:

```bash
export COPILOT_TOKEN=your_token_here
```

In Fish:

```fish
set -x COPILOT_TOKEN your_token_here
```

Then start the app with the `--copilot` flag:

```bash
cargo run --copilot chat
```

For a release build, add `--release`:

```bash
cargo run --release --copilot chat
```

The app uses `gpt-4o-mini` via the GitHub Models API.
