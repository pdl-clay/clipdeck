# Contributing to clipdeck

Thanks for helping out. This is a small, focused codebase — a single Rust binary — so
it's a comfortable place to land a first contribution.

## Getting set up

You need Rust 1.93+ and the Wayland development headers:

```sh
# Arch / CachyOS
sudo pacman -S --needed base-devel wayland libxkbcommon libinput seat

# Debian / Ubuntu
sudo apt install build-essential pkg-config libwayland-dev libxkbcommon-dev libinput-dev libseat-dev
```

Then:

```sh
git clone https://github.com/pdl-clay/clipdeck
cd clipdeck
cargo build --release
./target/release/clipdeck install
```

Press <kbd>Super</kbd>+<kbd>V</kbd> to open the popup.

## Before you push

CI runs exactly these three commands, so running them locally saves a round trip:

```sh
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test --release
```

## Adding a language

The interface strings are currently hardcoded in Portuguese across `src/app.rs` and
`src/view.rs`, with the CLI help text in `src/cli.rs`. Pulling those into a small
translation layer and adding an English catalogue is a genuinely welcome contribution —
the project is far more useful to more people once it isn't Portuguese-only.

## Things that are easy to get wrong

- **The shortcuts file is edited as text, not parsed.** `src/shortcut.rs` reads
  `~/.config/cosmic/com.system76.CosmicSettings.Shortcuts/v1/custom` and splices its own
  entry in, because the real format uses tuple keys that serialisation can't reproduce.
  It backs up before writing, writes atomically via a temp file, and is idempotent. If
  you touch it, keep those properties — the round-trip tests in that file cover them.
- **The daemon owns the clipboard.** Both the watcher and the popup need the
  `data-control` role, and there's one instance per session. Keep single-instance
  behaviour intact.
- **Pinned items bypass the history limit.** `evict` in `src/db.rs` and the tests around
  it enforce this; pinned items are exempt from both `max_items` and `max_total_bytes`.

## Commit messages

The existing history uses conventional prefixes — `feat:`, `fix:` — in the imperative
mood. Matching that keeps the log readable.

## Releasing

Maintainers only: push a `v*` tag and GitHub Actions builds the binaries, attaches them
to a draft release with generated notes, and publishes checksums.

```sh
git tag v0.1.0 && git push origin v0.1.0
```