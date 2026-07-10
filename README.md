# secretui

Native Rust TUI/CLI for Freedesktop Secret Service secrets.

## Development

Use project-local Cargo/Rustup state while working:

```bash
export CARGO_HOME=$PWD/.cargo-home
export RUSTUP_HOME=$PWD/.rustup
export CARGO_TARGET_DIR=$PWD/target
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
SECRETUI_INTEGRATION=1 cargo test --test integration_secret_service
```

## Commands

```bash
secretui
secretui export --metadata metadata.json
secretui import --metadata metadata.json
secretui backup --encrypted secrets.age
secretui restore --encrypted secrets.age --confirm-restore
secretui completions bash > secretui.bash
secretui man > secretui.1
```

Secrets are hidden by default. Reveal/copy/delete/restore require explicit user action. Clipboard clearing is best-effort because the desktop clipboard is outside this process.

## TUI keys

Pages flow `Collections → Items → Details`. `↑/↓` or `j/k` move, `Enter`/`→`/`l`/`Tab` go forward, `Esc`/`←`/`h` go back, `/` searches inline, and `?` shows help. On Collections, `n` creates a New Collection; on Items, `n` creates a New Item. On Details, use `↑/↓` to scroll, `←/→` to choose an action button, and `Enter` to activate it. Create/edit/delete use scrollable responsive in-TUI forms with bordered fields, visible cursor/focus, arrow-first navigation, and Save/Delete/Cancel buttons, not shell prompts. Attributes open their own single-column scrollable editor with Add/Update, Remove, Done, and Cancel buttons. Form elements keep readable minimum heights; hidden overflow is indicated by scrollbars.
