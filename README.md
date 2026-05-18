# yt2action 🎬 → 📋

Watch a **private YouTube playlist**, process new videos with AI, and automatically create:

- 📝 **Notes in Obsidian vault** — AI chooses the best folder (Learn/, Ideas/, Resources/, Health/, Things/) based on video content. Filenames are SEO-friendly slugs, while the original title is preserved inside the note.
- ✅ Optionally: **Tasks in Super Productivity** (configurable)

Built with **Rust 🦀** — single binary, zero runtime deps, runs quietly in background via cron.

## Features

- 🧠 **AI classification** — analyzes title + description + transcript to categorize each video
- 📁 **Smart folder routing** — AI picks the Obsidian folder, with case-insensitive fallback
- 🔗 **Clean Filenames** — automatically generates slugs for filenames (e.g., `mi-video-interesante.md`) while keeping the full YouTube title in the metadata and header.
- 🎨 **Fabric patterns** — optional deep analysis via community Fabric patterns (`--pattern extract_wisdom`)
- 📋 **Final report** — detailed summary of what was processed and where it was saved
- 🔄 **Playlist cleanup** — optionally move processed videos to a second playlist
- 🏷️ **Auto-tagging** — relevant keywords extracted from the content
- 📥 **Flexible Downloads** — fetches transcripts via `ytt` and optionally downloads the video via `yaydl`.
- 💾 **State tracking** — only processes new videos (use `--force` to reprocess)

## Quick Start

```bash
# 1. Place credentials.json in the project root
#    (Download from Google Cloud Console > OAuth 2.0 Client ID > Desktop App)

# 2. Initialize config
cargo run -- init

# 3. Edit config with your playlist ID and LLM endpoint
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
| **ytt & yaydl** | Transcript extraction and video downloading | Install via your package manager or GitHub |
| **Super Productivity** | Receives tasks via Local REST API | Settings → Misc → **Enable local REST API** (port 3876) |
| **LLM server** | AI processing: OpenAI-compatible API (llama.cpp, Ollama, OpenAI, etc.) | See [Configuration](#configuration) below |
| **Obsidian vault** (optional) | Notes are sorted into folders by theme | Set `obsidian_vault` in config |

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

# Reprocess already-processed videos
yt2action run --force

# Apply a Fabric pattern for deep content analysis
yt2action run --pattern extract_wisdom

# Combine options
yt2action run --limit 1 --force --pattern summarize

# Use a custom config file
yt2action run --config ~/.config/yt2action/custom.toml
```

### Fabric Patterns

You can apply any [Fabric pattern](https://github.com/danielmiessler/fabric) for rich content analysis:

```bash
# Extract wisdom from the video (IDEAS, INSIGHTS, QUOTES, etc.)
yt2action run --pattern extract_wisdom

# Get a structured summary
yt2action run --pattern summarize

# Analyze claims made in the video
yt2action run --pattern analyze_claims
```

The pattern output is appended to the Obsidian note under an "## Análisis Profundo" section.

### `yt2action list`

```bash
yt2action list
# 1. [12m] How to build a REST API in Rust — John Codes
# 2. [45m] Understanding Zero-Knowledge Proofs — Tech Deep Dive
# ...
```

### `yt2action init`

Creates `~/.config/yt2action/config.toml` with default values.

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

# Optional: move processed videos here
# processed_playlist_id = "PL_yyyyyyyyyyyyyyyyyy"

[processing]
# OpenAI-compatible API endpoint (llama.cpp, Ollama, OpenAI, etc.)
llm_base_url = "http://192.168.1.150:8080"
llm_model = "your-model-name"

# API key (optional — llama.cpp doesn't need one)
# llm_api_key = "sk-..."

[output]
obsidian_vault = "/home/leo/Memory/lenovo1"
sp_enabled = false

# Optional: override category → vault folder mapping
# [output.category_folders]
# tutorial = "Knowledge/Tutorials"
# health = "Wellness"
```

### LLM Support

yt2action uses the **OpenAI-compatible API** format (`/v1/chat/completions`), so it works with:

| Server | Example `llm_base_url` |
|--------|----------------------|
| **llama.cpp** | `http://192.168.1.150:8080` |
| **Ollama** | `http://localhost:11434` |
| **OpenAI** | `https://api.openai.com/v1` (+ set `llm_api_key`) |
| **Any OpenAI-compatible** | Point to your server |

## Cron Setup

```bash
# Every 6 hours, process the 3 newest videos with wisdom extraction
0 */6 * * * /usr/local/bin/yt2action run --limit 3 --pattern extract_wisdom >> ~/.yt2action.log 2>&1
```

## How It Works

```
You add video to private playlist 📥
        │
        ▼ (via cron or manual)
yt2action run
        │
       ├── YouTube Data API v3 → fetch playlist (OAuth2)
       ├── ytt → fetch transcript (saves to local transcript.txt)
       ├── yaydl → optionally download video (local storage)
       ├── Phase 1: AI classification → category + folder + tags
        │     (or via Fabric pattern if --pattern is set)
        └── Actions:
            ├── 📝 Obsidian: note in AI-chosen folder (slugified filename)
            ├── ✅ Optional: SP task
            └── 🔄 Optional: move to processed playlist
```

### AI Classification

1. **Phase 1 (always)**: The LLM analyzes title + description + transcript and returns structured JSON:
   - `summary` — 2-3 paragraphs in Spanish
   - `key_points` — actionable takeaways
   - `category` — tutorial, concept, tool, news, health, entertainment, other
   - `tags` — relevant keywords
   - `suggested_action` — watch_full, read_transcript, save_for_later, archive
   - `target_folder` — the best Obsidian folder for this content

2. **Phase 2 (optional)**: With `--pattern`, a Fabric pattern is applied for enriched analysis

### Category → Folder mapping (fallback)

If the AI doesn't return a valid folder, it falls back to:

| Category | Vault Folder | Example |
|----------|-------------|---------|
| Tutorial | `Learn/` | Rust tutorial, cooking class |
| Concept | `Ideas/` | ZKP explanation, mental model |
| Tool / News | `Resources/` | New CLI tool, tech announcement |
| Health | `Health/` | Exercise routine, nutrition |
| Entertainment | `Things/` | Vlog, music, comedy |
| Other | `Resources/YouTube/` | Misc fallback |

## Project Structure

```
src/
├── main.rs         → CLI (clap), orchestration, report, state management
├── config.rs       → TOML config loader
├── types.rs        → Video, ProcessedVideo, Classification, ProcessResult
├── youtube.rs      → YouTube Data API v3 + OAuth2 + playlist management
├── downloader.rs   → Transcript (ytt) and Video (yaydl) manager
├── processor.rs    → LLM client (OpenAI-compatible) + Fabric pattern support
├── sp_api.rs       → Super Productivity REST API client
└── obsidian.rs     → Markdown note writer for Obsidian vault
```

## License

MIT
