# secretui

**Inspect what applications actually stored.**

`secretui` is a fast, keyboard-driven browser, editor, and maintenance tool for the Linux Secret Service API. Browse collections, discover application-created entries, inspect attributes, and safely maintain credentials (without writing D-Bus calls or knowing the attributes in advance).

The project started when RustConn credentials existed in KWallet's Secret Service storage but were invisible in KWallet Manager. KWallet Manager uses the older KWallet-oriented view and may not expose generic Secret Service entries ([KDE discussion](https://discuss.kde.org/t/why-is-my-git-secret-not-visible-in-kwalletmanager-but-visible-in-seahorse-gui/43532)). That origin remains useful, but `secretui` is now positioned as a terminal-native administration and troubleshooting workspace rather than a replacement for every graphical password manager.

## Why secretui?

- `secret-tool` works well when you already know the attributes to query.
- [`lssecret`](https://github.com/gileshuang/lssecret) proves entries exist and lists their metadata.
- [KeepSecret](https://apps.kde.org/keepsecret/) provides a modern Secret Service-native graphical interface.
- `secretui` provides interactive terminal navigation, editing, troubleshooting, and deterministic metadata migration.

Its central differentiator is schema-neutral record maintenance: create, inspect, search, add, update, and remove arbitrary Secret Service attributes instead of assuming a fixed username/password shape.

It is aimed at developers, terminal users, minimal desktop environments, support workflows, and remote sessions that already have access to a Secret Service provider.

## Installation

Requirements: Linux and a running Freedesktop Secret Service provider (such as KDE Wallet, GNOME Keyring, or KeePassXC). Source builds also require Rust 1.97.0 and `cargo` available on `PATH`.

Prebuilt releases are available for x86-64 Linux. Download and verify one before
installing it into your user-local binary directory:

```bash
VERSION=v0.1.0
ARCHIVE="secretui-${VERSION}-x86_64-unknown-linux-gnu.tar.gz"
curl -fLO "https://github.com/edwordout/secretui/releases/download/${VERSION}/${ARCHIVE}"
curl -fLO "https://github.com/edwordout/secretui/releases/download/${VERSION}/SHA256SUMS"
sha256sum --check SHA256SUMS
tar -xzf "$ARCHIVE"
mkdir -p ~/.local/bin
install -m755 "${ARCHIVE%.tar.gz}/secretui" ~/.local/bin/secretui
secretui --version
```

The archive also includes the man page (`secretui.1`), Bash completion
(`secretui.bash`), README, and license files.

To build from source instead:

Build and install from this repository:

```bash
git clone https://github.com/edwordout/secretui.git
cd secretui
cargo install --path . --locked
secretui
```

For a standalone local build and install:

```bash
cargo build --release --locked
mkdir -p ~/.local/bin
install -m755 target/release/secretui ~/.local/bin/secretui
```

## Making a release

Maintainers should update the version in `Cargo.toml` and `Cargo.lock`, commit it,
and wait for CI on `main` to pass. Then create and push an annotated tag:

```bash
VERSION=v0.1.0
git tag -a "$VERSION" -m "Release $VERSION"
git push origin main
git push origin "$VERSION"
```

The tag must exactly match the package version. Before the first release, enable
immutable releases in the GitHub repository settings if that option is available.
If publishing fails after the draft is created, delete the draft before rerunning
the tag workflow.

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
