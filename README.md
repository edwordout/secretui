# secretui

**Inspect what applications actually stored.**

`secretui` is a fast, keyboard-driven browser, editor, and maintenance tool for the Linux Secret Service API. Browse collections, discover application-created entries, inspect attributes, and safely maintain credentials (without writing D-Bus calls or knowing the attributes in advance).

The project started when RustConn credentials existed in KWallet's Secret Service storage but were invisible in KWallet Manager. KWallet Manager uses the older KWallet-oriented view and may not expose generic Secret Service entries ([KDE discussion](https://discuss.kde.org/t/why-is-my-git-secret-not-visible-in-kwalletmanager-but-visible-in-seahorse-gui/43532)). That origin remains useful, but `secretui` is now positioned as a terminal-native administration and troubleshooting workspace rather than a replacement for every graphical password manager.

## Why secretui?

- `secret-tool` works well when you already know the attributes to query.
- [`lssecret`](https://github.com/gileshuang/lssecret) proves entries exist and lists their metadata.
- [KeepSecret](https://apps.kde.org/keepsecret/) provides a modern Secret Service-native graphical interface.
- `secretui` provides interactive terminal navigation, editing, troubleshooting, and deterministic metadata migration.

It is aimed at developers, terminal users, minimal desktop environments, support workflows, and remote sessions that already have access to a Secret Service provider.

## Installation

Requirements: Linux, a running Freedesktop Secret Service provider (such as KDE Wallet, GNOME Keyring, or KeePassXC), and Rust 1.97.0 for source builds.

Build and install from this repository:

```bash
git clone <repository-url>
cd secret_service_gui
cargo install --path . --locked
secretui
```

For a standalone local build:

```bash
cargo build --release --locked
./target/release/secretui
```

Prebuilt releases can install the `secretui` binary anywhere on `PATH`, for example `~/.local/bin/secretui`.

## Development

Use project-local Cargo/Rustup state while working:

```bash
export CARGO_HOME=$PWD/.cargo-home
export RUSTUP_HOME=$PWD/.rustup
export CARGO_TARGET_DIR=$PWD/target
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
SECRETUI_INTEGRATION=1 cargo test --test integration_secret_service -- --ignored
```

## Commands

```bash
secretui
secretui export --metadata metadata.json
secretui import --metadata metadata.json          # preview
secretui import --metadata metadata.json --apply  # update exact paths
secretui completions bash > secretui.bash
secretui man > secretui.1
```

Metadata exports use deterministic version 2 JSON and never retrieve secret values. Version 1 metadata can still be imported; its content type is ignored because Secret Service exposes content type only while retrieving a secret. Unknown versions are rejected.

Encrypted backup and restore are intentionally unavailable in v0.1 pending a design with scoped selection, conflict previews, streaming encryption, and safe restore identity.

Secrets are hidden by default. Reveal, copy, and delete require explicit user action. Copy accepts UTF-8 secrets; binary secrets are not silently converted. Clipboard clearing after 30 seconds is best-effort because the desktop clipboard is outside this process.

## TUI keys

Pages flow `Collections → Items → Details`. `↑/↓` or `j/k` move, `Enter`/`→`/`l`/`Tab` go forward, `Esc`/`←`/`h` go back, `/` searches inline, and `?` shows help. On Collections, `n` creates a New Collection; on Items, `n` creates a New Item. On Details, use `↑/↓` to scroll, `←/→` to choose an action button, and `Enter` to activate it. Create/edit/delete use scrollable responsive in-TUI forms with bordered fields, visible cursor/focus, arrow-first navigation, and Save/Delete/Cancel buttons, not shell prompts. Item forms include an attribute list with a final `+ Create new attribute` row; Enter opens a focused Add or Update editor, with Remove available while editing. Dirty forms are marked `(unsaved)` and Esc or Cancel asks whether to save, discard, or keep editing. Form elements keep readable minimum heights; hidden overflow is indicated by scrollbars.
