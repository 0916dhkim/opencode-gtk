# OpenCode GTK

A lightweight GTK 4 desktop client for a remote [OpenCode](https://opencode.ai) server. It is designed for Linux users who want a native window while keeping OpenCode and its projects on another machine.

**Requires OpenCode 2.x (2.0.8 or later).** The client speaks only the 2.x `/api/...` protocol and refuses 1.x servers with a clear error.

## Features

- Connects to `opencode serve` over HTTPS or loopback HTTP through an SSH tunnel
- Remembers the OpenCode password and the Cloudflare Access service token in the Linux system keyring
- Opens multiple OpenCode sessions as persistent tabs and restores them after restart
- Streams assistant text, reasoning, and tool activity over server-sent events, and resyncs after a reconnect
- Selects any model and reasoning variant exposed by the server; the choice is saved on the session by the server
- Creates and renames sessions in server-side project directories
- Sends file attachments (up to 20 MiB each) with prompts
- Answers permission requests, including those of subagent sessions, in place of the composer
- Sends messages while the agent works: steer them into the current run or queue them for after it
- Loads long conversations in pages

Prompts always use the server's default agent; there is no agent picker. Forms (input requests from tools, MCP servers or plugins) are not filled in here: a one-line notice above the composer names the waiting form and offers **Open web UI** and **Cancel** (`Ctrl+Shift+X`). The client never answers a form on its own.

### Steer and queue

The composer stays usable while a session runs. It then shows **Stop** and a **Steer** button: `Enter` (or **Steer**) steers the message into the current run, and the agent reads it at its next step without stopping; `Ctrl+Enter` (or **Queue for after** in the button's menu) queues it as a new turn once the run finishes. On an idle session both keys simply send.

Messages the server has not delivered yet wait in a tray above the composer, grouped in the order they will run: the steered ones together ("This run · at its next step"), then each queued one as its own turn ("After this run · Turn 2", "Turn 3", …). Each row can switch mode (**→ Queue** / **→ Steer**) or be cancelled (✕). Delivered messages move into the transcript.

**Stop** ends the run and parks every waiting message, steered or queued; nothing runs until you act. The tray then reads "Paused · N waiting", with the groups relabeled "Next turn", "Turn 2", …, and offers one **Resume**: it runs all of them in the order shown, the steered ones together in the next turn and then each queued one as its own turn. That is what the server does once a stopped session wakes, so rows only offer actions that do not wake it: ✕, and **→ Queue** on steered rows. To run only some messages, cancel the others first. Sending a new message to a stopped session wakes it too, so the composer warns about it while you type: the new message joins the next turn and the paused ones follow.

The client stores non-secret UI state under `${XDG_CONFIG_HOME:-~/.config}/opencode-gtk/state.json`. Tabs saved for sessions the server does not know (for example from an OpenCode 1.x server) are dropped quietly. The OpenCode Basic Auth password you enter in **Settings** and Cloudflare Access service tokens are stored by the desktop's Secret Service provider, such as GNOME Keyring or KWallet, and are never added to the state file or logs. When no Secret Service is available (or it is locked or fails), the password stays in memory for the session and the status bar shows a warning; connecting is never blocked.

## Server Setup

Run OpenCode 2.x on the remote machine. OpenCode 2.x always requires HTTP Basic auth with the username `opencode`; set its password with `OPENCODE_SERVER_PASSWORD`. The simplest secure setup keeps the server on loopback and reaches it through SSH:

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

The client adds `CF-Access-Client-Id` and `CF-Access-Client-Secret` to both API and event-stream requests, next to the OpenCode Basic auth header. Redirects remain disabled so credentials cannot be forwarded to another origin.

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

### Saved password

Type the password in **Settings** and press **Apply** to connect; with **Remember the password in the system keyring** checked (the default), it is saved and later launches connect without asking. The field then reads "Stored in the system keyring" and never shows the password; leave it blank to keep it, type a new one to replace it, or uncheck **Remember** and apply to remove it.

A saved password belongs to one server URL and username. The URL is normalized (scheme, host, port, and mount prefix; a trailing slash or `/api` does not matter), and switching to another server or username never sends it there: that connection uses its own saved password, if any.

At startup a `--password` or `OPENCODE_SERVER_PASSWORD` value takes precedence over the saved password, and is never written to the keyring unless you type it in **Settings**. A rejected password (401) keeps the saved entry; fix it in **Settings**.

## Shortcuts

| Shortcut | Action |
| --- | --- |
| `Enter` | Send prompt; steer it into the run while the session works |
| `Ctrl+Enter` | Queue the prompt for after the run (sends normally when idle) |
| `Shift+Enter` | Insert a newline |
| `Ctrl+T` | Create a session |
| `Ctrl+W` | Close the active tab |
| `Ctrl+Tab` | Select the next tab |
| `Ctrl+Shift+Tab` | Select the previous tab |
| `Ctrl+1` through `Ctrl+9` | Select a tab by position |
| `Ctrl+P` | Open sessions |
| `Ctrl+Q` | Quit |
| `F2` | Rename the active session |
| `Ctrl+U` | Attach files |
| `Ctrl+G` | Focus the composer |
| `Ctrl+M` | Choose a model |
| `Ctrl+/` | Choose a reasoning variant |
| `Ctrl+Shift+X` | Cancel the form shown in the notice |
| `Ctrl+B` | Toggle the sidebar |
| `Ctrl+,` | Open settings |
| `Escape` | Close the active modal |

Closing a tab does not delete or archive the server session. Reopen it at any time from **Sessions**.

## Development

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

`--preview` opens the real connected UI with canned OpenCode 2.x sessions, a permission request, a form, a running session with steered and queued messages waiting, and a stopped one with paused messages (Resume, and the composer's warning when you type), and no network:

```bash
cargo run -- --preview
```

UI tests run only headless (Xvfb and D-Bus, for example in Docker), never on a live desktop:

- `tests/smoke-ui.sh`: the window opens and survives the everyday shortcuts, in preview mode and against an unreachable server.
- `tests/remote-flow-ui.sh`: a full flow (bootstrap, paging, rename, create, prompt with an attachment, steer, queue, tray switch/cancel, Stop and Resume (with a parked steer, and with queued messages only), the paused composer warning, permissions, form cancel, reconnects) against `tests/fake_opencode_server.py`, a fake 2.x server built from real 2.0.8 captures. `python3 tests/fake_v2/selftest.py` checks the fake server itself.
- `tests/keyring-ui.sh`: the password saved in **Settings** lands in a real Secret Service (gnome-keyring, installed at test time; see the script header), survives app and keyring restarts, is removed by unchecking **Remember**, is never saved from `OPENCODE_SERVER_PASSWORD`, and the client still connects without a session bus.
- `tests/v2/e2e.sh --state DIR`: runs against a real, isolated OpenCode 2.0.8 server with a scripted mock model provider in Docker (`tests/v2/README.md`). It runs the ignored live API test (`cargo test live_server_end_to_end -- --ignored`) and a GUI smoke test, then takes the server down. It builds its Docker image from the published CLI, so CI does not run it.

The included `Dockerfile` provides a reproducible Debian build environment when GTK development libraries are not installed locally:

```bash
docker build -t opencode-gtk .
docker run --rm opencode-gtk
```

The container's default command runs the complete Rust test suite.

## Scope

OpenCode GTK deliberately uses the public OpenCode HTTP API instead of embedding the CLI or terminal UI. It focuses on the everyday chat loop. Sharing, reverting, forking, compaction, and a viewer for background subagent sessions are not exposed yet.

## License

[MIT](LICENSE)
