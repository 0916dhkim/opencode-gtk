# OpenCode GTK

A lightweight GTK 4 desktop client for a remote [OpenCode](https://opencode.ai) server. It is designed for Linux users who want a native window while keeping OpenCode and its projects on another machine.

## Features

- Connects to `opencode serve` over HTTPS or loopback HTTP through an SSH tunnel
- Authenticates to Cloudflare Access with a service token stored in the Linux system keyring
- Opens multiple OpenCode sessions as persistent tabs
- Restores open tabs and per-session model choices after restart
- Streams assistant text, reasoning, and tool activity over server-sent events
- Selects any provider, model, and reasoning variant exposed by the server
- Creates sessions in server-side project directories
- Sends file attachments with prompts
- Handles permission requests and agent questions in native dialogs
- Loads long conversations in pages and reconnects the event stream automatically

The client stores non-secret UI state under `${XDG_CONFIG_HOME:-~/.config}/opencode-gtk/state.json`. OpenCode Basic Auth passwords stay in memory. Cloudflare Access service tokens are stored by the desktop's Secret Service provider, such as GNOME Keyring or KWallet, and are never added to the state file.

## Server Setup

Run OpenCode on the remote machine. The simplest secure setup keeps it on loopback and reaches it through SSH:

```bash
OPENCODE_SERVER_PASSWORD='choose-a-password' opencode serve \
  --hostname 127.0.0.1 \
  --port 4096
```

Create the tunnel from the Linux desktop:

```bash
ssh -N -L 4096:127.0.0.1:4096 user@remote-host
```

Then connect the client to `http://127.0.0.1:4096`.

For a directly reachable server, put OpenCode behind HTTPS. OpenCode GTK refuses every non-loopback HTTP address because prompts, responses, project metadata, and attachments would otherwise travel without encryption.

### Cloudflare Access

For an OpenCode server published through Cloudflare Tunnel:

1. Protect the hostname with a Cloudflare Access self-hosted application.
2. Create a service token under **Zero Trust > Access controls > Service credentials**.
3. Add a **Service Auth** policy to the application that includes that token.
4. In OpenCode GTK, open **Settings**, enter the HTTPS server URL, and paste the service token's Client ID and Client Secret.

The client adds `CF-Access-Client-Id` and `CF-Access-Client-Secret` to both API and event-stream requests. Redirects remain disabled so credentials cannot be forwarded to another origin.

## Linux Dependencies

Install Rust from [rustup.rs](https://rustup.rs), then install GTK 4 development packages for your distribution.

Ubuntu or Debian:

```bash
sudo apt install build-essential libdbus-1-dev libgtk-4-dev pkg-config
```

Fedora:

```bash
sudo dnf install dbus-devel gcc gtk4-devel pkgconf-pkg-config
```

Arch Linux:

```bash
sudo pacman -S base-devel dbus gtk4
```

## Install

```bash
git clone https://github.com/0916dhkim/opencode-gtk.git
cd opencode-gtk
./install.sh
```

The installer builds a release binary, places it at `~/.local/bin/opencode-gtk`, and creates a desktop entry. Set `CARGO_INSTALL_ROOT` (or `PREFIX`) to choose another binary prefix and `XDG_DATA_HOME` to choose another desktop-entry location.

You can also run from source:

```bash
cargo run --release -- --server http://127.0.0.1:4096
```

## Connect

Launch **OpenCode GTK** from your application menu and open **Settings**, or pass connection settings on the command line:

```bash
opencode-gtk \
  --server https://opencode.example.com \
  --username opencode \
  --password 'your-password'
```

Environment variables are preferable to command-line passwords because command arguments may be visible to other local processes:

```bash
export OPENCODE_SERVER_URL=https://opencode.example.com
export OPENCODE_SERVER_USERNAME=opencode
export OPENCODE_SERVER_PASSWORD='your-password'
opencode-gtk
```

Cloudflare Access credentials can also be supplied for one run with `OPENCODE_CF_ACCESS_CLIENT_ID` and `OPENCODE_CF_ACCESS_CLIENT_SECRET`. Enter them in **Settings** instead when you want the system keyring to retain them.

`OPENCODE_SERVER_URL` defaults to `http://127.0.0.1:4096`, and the username defaults to `opencode`.

## Shortcuts

| Shortcut | Action |
| --- | --- |
| `Enter` | Send prompt |
| `Shift+Enter` | Insert a newline |
| `Ctrl+T` | Create a session |
| `Ctrl+W` | Close the active tab |
| `Ctrl+Tab` | Select the next tab |
| `Ctrl+Shift+Tab` | Select the previous tab |
| `Ctrl+1` through `Ctrl+9` | Select a tab by position |
| `Ctrl+U` | Attach files |
| `Ctrl+,` | Open settings |

Closing a tab does not delete or archive the server session. Reopen it at any time from **Sessions**.

## Development

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

The included `Dockerfile` provides a reproducible Debian build environment when GTK development libraries are not installed locally:

```bash
docker build -t opencode-gtk .
docker run --rm opencode-gtk
```

The container's default command runs the complete Rust test suite.

## Scope

OpenCode GTK deliberately uses the public OpenCode HTTP API instead of embedding the CLI or terminal UI. The first release focuses on the everyday chat loop. Advanced TUI operations such as sharing, reverting, forking, and compaction are not yet exposed.

## License

[MIT](LICENSE)
