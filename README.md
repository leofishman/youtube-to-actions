# yt2action 🎬 → 📋

Watch a **private YouTube playlist**, process new videos with AI (Ollama), and automatically create:

- 📝 **Notes in Obsidian vault** — categorized into Learn/, Ideas/, Resources/, Health/ or Things/ depending on the video content
- ✅ Optionally: **Tasks in Super Productivity** (configurable)

Built with **Rust 🦀** — single binary, ~5MB, zero runtime deps, runs quietly in background via cron.

## Quick Start

```bash
# 1. Place credentials.json in the project root
#    (Download from Google Cloud Console > OAuth 2.0 Client ID > Desktop App)

# 2. Initialize config
cargo run -- init

# 3. Edit config with your playlist ID
vim ~/.config/yt2action/config.toml

# 4. Authenticate with Google (opens browser — only needed once)
yt2action auth

# 5. Check connections
yt2action health

# 6. List videos in your playlist
yt2action list

# 7. Run the pipeline
yt2action run --limit 3

# Or build and install the binary:
cargo build --release
sudo cp target/release/yt2action /usr/local/bin/
```

## Prerequisites

| What | Why | How |
|------|-----|-----|
| **Google Cloud project** | OAuth 2.0 access to your private YouTube playlists | [Google Cloud Console](https://console.cloud.google.com) → APIs & Services → Credentials → Create OAuth client ID (Desktop App) |
| **`credentials.json`** | OAuth client secret — place in project root | Downloaded during OAuth client creation above |
| **YouTube Data API v3** | Must be enabled for your project | [Enable here](https://console.cloud.google.com/apis/library/youtube.googleapis.com) |
| **Super Productivity** | Receives tasks via Local REST API | Settings → Misc → **Enable local REST API** (port 3876) |
| **Ollama** (optional) | AI processing: summaries + classification | `ollama pull llama3.2` — or configure a remote API in config |
| **Obsidian vault** (optional) | Notes are written to `Resources/YouTube/` | Set `obsidian_vault` in config |

## Commands

```
Usage: yt2action <COMMAND>

Commands:
  auth    Authenticate with Google OAuth (opens browser)
  run     Full pipeline: fetch playlist → process → create tasks/notes
  list    List videos in playlist (without processing)
  init    Create default config file at ~/.config/yt2action/
  health  Test SP API connection
  help    Print help
```

### `yt2action auth`

Opens your browser for Google OAuth consent. Only needed **once** — the token is cached in `token_cache.json`.

```bash
yt2action auth
# A browser window will open → sign in → authorize → done.
```

### `yt2action run`

```bash
# Process the 5 newest unprocessed videos
yt2action run

# Process only the 3 newest
yt2action run --limit 3

# Use a custom config file
yt2action run --config ~/.config/yt2action/custom.toml
```

### `yt2action list`

```bash
yt2action list
# 1. [12m] How to build a REST API in Rust — John Codes
# 2. [45m] Understanding Zero-Knowledge Proofs — Tech Deep Dive
# ...
```

### `yt2action init`

Creates `~/.config/yt2action/config.toml` with default values. Edit it with your playlist ID.

### `yt2action health`

```bash
yt2action health
# 📋 Super Productivity...
#    ✅ API responding (port 3876).
```

## Configuration

File: `~/.config/yt2action/config.toml`

```toml
[youtube]
credentials_path = "credentials.json"
playlist_id = "PL_xxxxxxxxxxxxxxxxxxxx"

[processing]
ollama_url = "http://localhost:11434"
ollama_model = "llama3.2"

[output]
obsidian_vault = "/home/leo/Memory/lenovo1"
sp_enabled = false
```

## Cron Setup

```bash
# Every 6 hours, process the 3 newest videos
0 */6 * * * /usr/local/bin/yt2action run --limit 3 >> ~/.yt2action.log 2>&1
```

Or via **Hermes Agent**:

```bash
hermes cron create \
  --schedule "0 */6 * * *" \
  --prompt "Ejecuta yt2action run --limit 3"
```

## How It Works

```
You add video to private playlist 📥
        │
        ▼ (every 6h via cron)
yt2action run
        │
        ├── YouTube OAuth → fetch new videos
        ├── youtube-transcript → get CC captions
        ├── Ollama → summarize + classify + tag
        └── Actions:
            ├── 📝 Obsidian: note in category folder
            └── ✅ Optional: SP task
```

### Category → Folder mapping

| Video Category | Vault Folder | Example |
|---------------|-------------|---------|
| Tutorial | `Learn/` | Rust tutorial, cooking class |
| Concept | `Ideas/` | ZKP explanation, mental model |
| Tool / News | `Resources/` | New CLI tool, tech announcement |
| Health | `Health/` | Exercise routine, nutrition |
| Entertainment | `Things/` | Vlog, music, comedy |
| Other | `Resources/YouTube/` | Misc fallback |

## Project Structure

```
src/
├── main.rs         → CLI (clap), orchestration, state management
├── config.rs       → TOML config loader
├── types.rs        → Video, ProcessedVideo, Classification, SpTask
├── youtube.rs      → YouTube Data API v3 + OAuth2 (yup-oauth2)
├── transcript.rs   → CC transcript fetcher (youtube-transcript crate)
├── processor.rs    → Ollama/LLM: summary, key points, classification
├── sp_api.rs       → Super Productivity REST API client
└── obsidian.rs     → Markdown note writer for Obsidian vault
```

## License

MIT
