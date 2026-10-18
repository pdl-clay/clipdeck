<div align="center">

# clipdeck

**Clipboard history for the COSMIC desktop.**
Press <kbd>Super</kbd>+<kbd>V</kbd>, search what you copied, paste it straight into the app you were using.

[![license](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
![rust](https://img.shields.io/badge/rust-1.93%2B-orange.svg)
![wayland](https://img.shields.io/badge/wayland-only-purple.svg)

[English](README.md) · [Português](README.pt-BR.md)

</div>

---

![clipdeck popup](docs/screenshot.png)

## Why

COSMIC is a great desktop, but it has no clipboard history. Copy something, want it
back three steps later, and it's gone — only the current selection survives.

Other Wayland desktops solve this with a separate clipboard daemon. clipdeck does it
natively, as a single Rust binary:

- **No third-party daemon, no service, no network.**
- **Watches the clipboard without needing focus**, via
  `zwlr_data_control_manager_v1`.
- **Pastes into the app you were already using**, by injecting the paste key through
  the compositor's virtual keyboard.
- **Follows the COSMIC theme**, including the frosted-glass mode.

## Features

- <kbd>Super</kbd>+<kbd>V</kbd> popup with full-text search (accent- and
  case-insensitive, prefix matching)
- Keyboard-first: arrows/Tab to move, <kbd>Enter</kbd> to paste, no mouse required
- Pin items so they survive the history limit
- Rich previews: images at full size, text and files inline, HTML stripped to plain text
- HTML items are offered as **both** `text/html` and `text/plain`, so pasting into a
  text field or terminal gives clean text, not markup
- Deduplicated by SHA-256 — copying the same thing twice moves the item to the top
  instead of duplicating it
- Respects `x-kde-passwordManagerHint: secret`, so password managers are never recorded
- SQLite history that survives reboots, with size and count caps
- A full CLI for scripting and `doctor` for diagnosing setup problems

## Requirements

- **COSMIC** (tested on 1.8.0) on Wayland
- The compositor must expose `zwlr_data_control_manager_v1`, `zwlr_layer_shell_v1`
  and `zwp_virtual_keyboard_manager_v1` — run `clipdeck doctor` to check
- Dynamically linked against glibc (built and tested on CachyOS / Arch)

> Prebuilt release binaries are **x86_64** for now. On other architectures, build
> from source — it works, it just takes a few minutes.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/pdl-clay/clipdeck/main/install.sh | bash
```

That downloads the latest release, verifies its checksum, installs the binary to
`~/.local/bin`, registers the autostart entry and adds the <kbd>Super</kbd>+<kbd>V</kbd>
shortcut. It prints what it did and where.

If you'd rather point at a specific version, or skip the shortcut registration:

```sh
curl -fsSL https://raw.githubusercontent.com/pdl-clay/clipdeck/main/install.sh | bash -s -- --version v0.1.0
curl -fsSL https://raw.githubusercontent.com/pdl-clay/clipdeck/main/install.sh | bash -s -- --no-setup
```

### From source

You need Rust 1.93+ and the Wayland development headers:

```sh
# Arch / CachyOS
sudo pacman -S --needed base-devel wayland libxkbcommon libinput seat

# Debian / Ubuntu
sudo apt install build-essential pkg-config libwayland-dev libxkbcommon-dev libinput-dev libseat-dev

git clone https://github.com/pdl-clay/clipdeck
cd clipdeck
cargo install --path . --locked
clipdeck install
```

## Usage

| Action | Result |
|---|---|
| <kbd>Super</kbd>+<kbd>V</kbd> | open / close the popup |
| Type | search the history |
| <kbd>↑</kbd> <kbd>↓</kbd> or <kbd>Tab</kbd> | move between items |
| <kbd>Enter</kbd> | put the item on the clipboard and paste it |
| <kbd>Shift</kbd>+<kbd>Enter</kbd> | paste as plain text (ignores HTML markup) |
| Click an item | select it and show the **preview** (does not paste) |
| Double-click an item | paste it and close |
| Click outside | close |
| <kbd>Ctrl</kbd>+<kbd>P</kbd> | pin / unpin — pinned items never age out |
| <kbd>Delete</kbd> | remove the item |
| <kbd>PageUp</kbd> / <kbd>PageDown</kbd> | scroll long previews |
| <kbd>Esc</kbd> | close |

> The interface currently ships in Portuguese. Contributions to localise it are very
> welcome.

## CLI

```sh
clipdeck list [--limit N] [--json]   # recent items
clipdeck search <term>              # full-text search
clipdeck copy <id> [--plain]        # put an item back on the clipboard
clipdeck pin <id> | clipdeck unpin <id>
clipdeck remove <id>
clipdeck clear [--keep-pinned] --yes
clipdeck status [--json]            # database and config state
clipdeck doctor                     # diagnose protocols, shortcut, virtual keyboard
clipdeck key ctrl+v | super+v | up | down | enter | escape | delete | tab
clipdeck install | clipdeck uninstall [--purge]
```

`clipdeck key` sends a key combination to the focused window without stealing focus —
handy for scripting.

## Configuration

`~/.config/clipdeck/config.toml` — the file is optional; these are the defaults:

```toml
max_items        = 500          # history length (pinned items don't count)
max_total_bytes  = 268435456    # total cap across all items (256 MiB)
max_item_bytes   = 20971520     # per-item cap (20 MiB)
max_text_bytes   = 1048576      # cap for text content (1 MiB)
auto_paste       = true         # paste automatically after picking
paste_delay_ms   = 150          # wait for focus to return to the previous app
popup_width      = 1000
popup_height     = 460
```

Popup and paste settings apply the next time the popup opens; the capture limits
(`max_*`) apply from the next daemon start.

## How it works

- **Capture** — a dedicated thread connects to Wayland through
  `zwlr_data_control_manager_v1`, which observes the clipboard without focus. On each
  new selection it picks the best available MIME type —
  `text/uri-list` → image → `text/html` (with its `text/plain` companion) → text — and
  writes it to SQLite.
- **Dedupe** — SHA-256 of the content. Re-copying the same thing bumps it to the top.
- **Popup** — one instance per D-Bus activation. The window is a transparent
  full-screen `zwlr_layer_shell_v1` surface: the panel is centred, the theme's blur is
  applied only to the panel rectangle, and the rest of the surface catches the
  click-outside that closes it. Because the surface covers the output, the pointer never
  leaves it, so the compositor can't hand focus to another window mid-interaction.
- **Paste** — the selection is published by clipdeck itself through
  `zwlr_data_control_manager_v1`, one content per MIME type. With `auto_paste`, a
  <kbd>Ctrl</kbd>+<kbd>V</kbd> is injected via the compositor's virtual keyboard once the
  popup closes. If injection fails the item stays on the clipboard — just paste by hand.
- **Images** — a 256 px thumbnail for the list and a 1024 px preview for the side panel
  are stored in the database; the original BLOB is what goes to the clipboard.

## Privacy

- Everything stays on your machine. Nothing is sent anywhere.
- Password-manager selections marked `x-kde-passwordManagerHint: secret` are never
  recorded.
- History lives in `~/.local/share/clipdeck/history.db` (SQLite in WAL mode). Delete it
  any time with `clipdeck clear` or `clipdeck uninstall --purge`.

## Known limitations

- Regular clipboard only — the X11 primary selection is not observed.
- Auto-paste injects <kbd>Ctrl</kbd>+<kbd>V</kbd>. Terminals usually use
  <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>V</kbd>, so use the terminal's own paste
  shortcut there, or turn `auto_paste` off and paste manually.
- Auto-paste depends on focus returning to the previous app; tune `paste_delay_ms` on
  slower setups.
- Apps that own the selection and exit immediately can hand over empty content. Those
  copies are skipped — if that's your case, pair clipdeck with `wl-clip-persist`.

## Troubleshooting

```sh
clipdeck doctor
```

Checks the required Wayland protocols, whether the compositor will let clipdeck own the
clipboard, virtual keyboard availability, the autostart entry and the
<kbd>Super</kbd>+<kbd>V</kbd> shortcut.

If the shortcut doesn't fire, note that the popup also closes on <kbd>Super</kbd>+<kbd>V</kbd>
when it's already open. You can also bind it by hand in
**Settings → Keyboard → Custom Shortcuts** with the command `clipdeck --toggle`.

## Uninstall

```sh
clipdeck uninstall            # removes autostart and the shortcut, keeps history
clipdeck uninstall --purge    # also deletes the database
```

## Contributing

Issues and pull requests are welcome — see [CONTRIBUTING.md](CONTRIBUTING.md).

```sh
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test --release
```

Tagged pushes (`v*`) build and publish release binaries through GitHub Actions.

## License

[MIT](LICENSE) © Luan Rhiston