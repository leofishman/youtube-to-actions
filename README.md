# yt2action 🎬 → 📋

Watch a private YouTube playlist, process new videos with AI (Ollama), and automatically create:
- ✅ **Tasks in Super Productivity** (with summary, tags, and links)
- 📝 **Notes in Obsidian vault** (with YAML frontmatter, key points, and transcript)

Built with Rust 🦀 — single binary, zero runtime deps, runs quietly in background via cron.

## Quick Start

```bash
# 1. Initialize config
yt2action init

# 2. Edit config with your YouTube API key and playlist ID
vim ~/.config/yt2action/config.toml

# 3. Test connection
yt2action health

# 4. List videos in playlist
yt2action list

# 5. Run the pipeline
yt2action run

# Or process only the 3 newest:
yt2action run --limit 3
```

## Configuration

See `yt2action init` or copy `config.example.toml` to `~/.config/yt2action/config.toml`.

### Prerequisites

- **YouTube Data API v3 key** — get one free at [Google Cloud Console](https://console.cloud.google.com)
- **Ollama** running locally with a model (e.g. `llama3.2`) — or an OpenAI-compatible API
- **Super Productivity** with Local REST API enabled (Settings → Misc → Enable local REST API)
- **Obsidian** vault (optional — for note generation)

## Usage

```
Usage: yt2action <COMMAND>

Commands:
  run     Run the full pipeline
  init    Create default config file
  list    List videos in playlist
  health  Test API connections
  help    Print help
```

## Cron Setup

To run automatically every 6 hours:

```bash
crontab -e
# Add:
0 */6 * * * /usr/local/bin/yt2action run --limit 5 >> ~/.yt2action.log 2>&1
```

Or with Hermes Agent:

```bash
hermes cron create \
  --schedule "0 */6 * * *" \
  --prompt "Ejecuta yt2action run --limit 5 en ~/Projects/youtube-to-actions"
```

## Architecture

```
yt2action
├── youtube.rs     → YouTube Data API v3 (playlist items + video durations)
├── transcript.rs  → youtube-transcript crate (CC captions)
├── processor.rs   → Ollama/LLM (summary + classification)
├── sp_api.rs      → Super Productivity REST API (task creation)
├── obsidian.rs    → Markdown note writer
├── config.rs      → TOML config management
└── types.rs       → Shared data types
```

## License

MIT
