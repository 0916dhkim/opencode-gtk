# OpenCode Desktop (COSMIC)

A native COSMIC desktop client for a remote [OpenCode](https://opencode.ai) server, built with [`libcosmic`](https://github.com/pop-os/libcosmic) and `iced`. Designed for Linux / COSMIC Desktop users who want a first-class Wayland desktop experience with pure-Rust performance and memory safety.

**Requires OpenCode 2.x (2.0.8 or later).**

## Features

- **Native COSMIC Integration**: Built on `libcosmic` with system palette tokens, Wayland support, and responsive drawer navigation.
- **Connection & Security**: Remembers OpenCode passwords and Cloudflare Access tokens in the Linux system keyring (Secret Service).
- **Session Tabs & History**: Multiple active sessions in tabs, fast switching, and a session search drawer.
- **Streaming Transcripts**: Real-time streaming over Server-Sent Events (SSE) for assistant text, reasoning, and tool executions.
- **Rich Markdown**: Code blocks with language detection and one-click copy to clipboard, formatted headings, lists, blockquotes, and tables.
- **Steer & Queue Composer**: Steer prompts into active runs at the next turn, queue follow-ups, or stop/park running sessions.
- **Waiting Prompts Tray**: Interactive tray above composer with switch and cancel controls.
- **Background Jobs Drawer**: Live visibility into active session background subagents and shell commands with live elapsed times.

## Building & Running

### Dependencies

Requires Rust 1.93+ (Rust 2024 edition).

Build for Linux:

```bash
cargo build --release
```

Or run offline preview mode without connecting to a server:

```bash
cargo run -- --preview
```

### CLI Flags

```
--server <URL>               OpenCode server URL
--username <USER>            HTTP Basic Auth username (default: opencode)
--password <PASS>            HTTP Basic Auth password
--cf-access-client-id <ID>   Cloudflare Access client ID
--cf-access-client-secret    Cloudflare Access client secret
--preview                    Launch offline mock preview UI
```

## Keyboard Shortcuts

| Key | Action |
| --- | --- |
| `Ctrl+T` | New session |
| `Ctrl+W` | Close the active tab |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | Next / previous tab |
| `Ctrl+1` … `Ctrl+9`, `Alt+1` … `Alt+9` | Select a tab by position |
| `Ctrl+B` | Fold the sidebar |
| `Ctrl+P` | Session search drawer |
| `Ctrl+,` | Settings drawer |
| `Ctrl+G` | Put the caret back in the prompt composer |
| `Enter` | Send; steers into the run while a session is running |
| `Ctrl+Enter` | Queue a follow-up turn while a session is running |
| `Escape` | Close the open drawer |
