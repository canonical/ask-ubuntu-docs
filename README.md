# Ask the Docs

An LLM-based help app that uses the official Ubuntu documentation.

This app can answer your queries about Ubuntu based on the documentation database created with [ubuntu-docs-indexer](https://github.com/msuchane/ubuntu-docs-indexer/). The chat is powered by a local or cloud LLM. You can choose at startup:

- A local model of your choice served through Ollama. Note that local models are resource-intensive. For example, `deepseek-r1:1.5b` requires at least 2 GB of RAM and ~2 GB of disk space.
- A remote model via GitHub Copilot and the GitHub Models API. A free GitHub account is enough.

The app is written in Rust with the help of Copilot and Claude. It has a GTK4 graphical interface and a command-line interface.


## Run the app with GitHub Copilot

Use this mode for a remote model via the GitHub Models API.

This can feel faster than running a local model because the inference work does
not happen on your machine, though the result still depends on network latency.

The app uses the `gpt-4o-mini` model via the GitHub Models API.

1. Install Ask the Docs from Snapcraft:

    ```bash
    sudo snap install ask-ubuntu-docs
    ```

2. Install the GitHub CLI:

    ```bash
    sudo snap install gh
    ```

3. Authenticate with GitHub:

    ```bash
    gh auth login
    ```

4. Verify the authentication:

    ```bash
    gh auth token
    ```

5. Run Ask the Docs from your desktop interface, or launch it from the command line:

    ```bash
    ask-ubuntu-docs gui --copilot
    ```

### Copilot token

Alternatively, you can set `COPILOT_TOKEN` manually. This takes precedence over the GitHub CLI.

1. Generate a token at https://github.com/settings/tokens with the `models:read` scope, or use a token from an existing GitHub Copilot subscription.

2. Set your token as an environment variable:

    In Bash:
    
    ```bash
    export COPILOT_TOKEN=your_token_here
    ```
    
    In Fish:
    
    ```fish
    set -x COPILOT_TOKEN your_token_here
    ```

3. Run the app from the command line:

    ```bash
    ask-ubuntu-docs gui --copilot
    ```

    Note that if you run the app from the desktop launcher or a different terminal session, it won't detect your token variable.


## Run the app with a local model

The app talks to a local model through Ollama. You must choose and install the model separately.

1. Install Ask the Docs from Snapcraft:

    ```bash
    sudo snap install ask-ubuntu-docs
    ```

2. Choose a model from the [Ollama library](https://ollama.com/library). In this example, we're using the small, text-optimized `deepseek-r1:1.5b` model. This is currently the default.

3. Install Ollama:

    ```bash
    sudo snap install ollama
    ```

4. Download your chosen model:

    ```bash
    ollama pull deepseek-r1:1.5b
    ```

5. Start the app, passing the chosen model:

    ```bash
    ask-ubuntu-docs gui --model deepseek-r1:1.5b
    ```

### Using environment variables

You can also set the model and server URL via environment variables. For example:

```bash
MODEL=deepseek-r1:1.5b OLLAMA_URL=http://localhost:11434 ask-ubuntu-docs gui
```


## Build the app

1. Install the Rust toolchain:

    ```bash
    sudo snap install rustup --classic
    rustup toolchain install stable
    ```

2. Compile the app in debug mode (faster build):

    ```bash
    cargo build
    ```

3. Download the latest `index.lance.tar.gz` archive:

    https://github.com/msuchane/ubuntu-docs-indexer/releases

4. Unpack the archive and place the resulting directory under `target/`.

5. Make sure that you're authenticated with `gh`. See earlier.

6. Run the app with Copilot and the graphical interface:

    ```bash
    cargo run gui --copilot
    ```
