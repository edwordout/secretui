# secretui

**Inspect what applications actually stored.**

`secretui` is a keyboard-driven administration and troubleshooting tool for the
Linux Secret Service API. It browses collections, inspects arbitrary item
metadata, and performs deliberate maintenance without assuming a
username/password schema.

![secretui Collections screen showing a KDE Wallet collection](ss.png)

*Browse Secret Service collections and credentials without leaving the terminal.*

The project began when application credentials stored through KWallet's Secret
Service interface were not visible in KWallet Manager. It remains an
administration workspace, **not a replacement password manager**.

## v0.1 release contract

SecretUI promises to:

- browse collection and item metadata without retrieving secret values;
- retrieve a secret only after an explicit Reveal or Copy action;
- keep label and attribute edits separate from secret replacement;
- restore lock state after a temporary unlock during ordinary success and
  failure paths, and report cleanup failures;
- plan and apply **same-store metadata repair** by exact provider object path;
- provide operation and target context for provider errors; and
- keep secret values out of metadata exports, recovery files, apply reports,
  rendered errors, and the in-memory store's operation log.

SecretUI does **not** promise:

- password-manager features, encrypted secret backup, or secret migration;
- recovery of a deleted secret (metadata exports cannot restore one);
- cross-provider or cross-database metadata matching;
- compatibility with every Secret Service implementation; or
- protection from root, privileged or same-user processes, a compromised
  provider/desktop/terminal, clipboard history, screen capture, crash dumps, or
  physical access to an unlocked session.

See [SECURITY.md](SECURITY.md) for the threat model and private reporting route.

## Why secretui?

- `secret-tool` works well when the attributes to query are already known.
- [`lssecret`](https://github.com/gileshuang/lssecret) lists stored metadata.
- [KeepSecret](https://apps.kde.org/keepsecret/) provides a graphical Secret
  Service-native interface.
- `secretui` provides terminal navigation, schema-neutral metadata editing,
  lock-aware secret actions, and deterministic same-store metadata repair.

This is useful for developers, terminal users, support workflows, minimal
desktops, and remote sessions that already have access to a Secret Service
provider. For unattended or boot-time credentials, use a purpose-built system
such as [systemd credentials](https://systemd.io/CREDENTIALS/), Vault, a cloud
secret manager, or the platform's orchestrator secrets.

## Runtime and compatibility

SecretUI requires:

- Linux on x86-64 for the published binary;
- a working user session D-Bus;
- an active Freedesktop Secret Service provider;
- a modern UTF-8 terminal with alternate-screen and keyboard-event support; and
- access to a graphical session when the provider needs to show an unlock or
  authorization prompt.

It does not create a session bus, start a provider, or make graphical provider
prompts work in a headless session. It communicates over D-Bus through the Rust
[`secret-service`](https://docs.rs/secret-service/latest/secret_service/) crate
and does not dynamically link to C `libsecret`.

The v0.1.2 `x86_64-unknown-linux-gnu` artifact is a dynamically linked binary
built by GitHub Actions on Ubuntu 22.04. It requires glibc 2.34 or newer and is
not a universal/static Linux build.

The required v0.1.2 live compatibility gate targets Ubuntu 26.04/KDE with
KWallet 6.24.0. The latest unattended run reached the provider but its graphical
prompt was dismissed, so it did **not** satisfy the release gate. KWallet,
GNOME Keyring, and KeePassXC remain **unverified** for v0.1.2 until the isolated
test passes with an authorized prompt. Provider behavior can differ; please
report failures using synthetic metadata only.

## Installation

### Prebuilt x86-64 binary

Download the archive and checksum from the same GitHub release, verify them,
then install the binary for the current user:

```bash
VERSION=v0.1.2
ARCHIVE="secretui-${VERSION}-x86_64-unknown-linux-gnu.tar.gz"
curl -fLO "https://github.com/edwordout/secretui/releases/download/${VERSION}/${ARCHIVE}"
curl -fLO "https://github.com/edwordout/secretui/releases/download/${VERSION}/SHA256SUMS"
sha256sum --check SHA256SUMS
tar -xzf "$ARCHIVE"
mkdir -p ~/.local/bin
install -m755 "${ARCHIVE%.tar.gz}/secretui" ~/.local/bin/secretui
secretui --version
```

The archive also includes `secretui.1`, Bash completion, the metadata-format
and release documents, the security policy, changelog, README, image, and
licenses. Verify that `~/.local/bin` is on `PATH`.

### Build from source

Source builds require Rust 1.97.0 and Cargo:

```bash
git clone https://github.com/edwordout/secretui.git
cd secretui
cargo install --path . --locked
secretui --version
```

### Uninstall

For either installation method:

```bash
cargo uninstall secretui 2>/dev/null || true
rm -f ~/.local/bin/secretui
```

Also remove any man page or completion file you installed manually. SecretUI
does not create a configuration directory. Keep or securely remove metadata,
recovery, and report JSON files according to your local retention policy.

## Commands

```bash
secretui
secretui export --metadata metadata.json
secretui export --metadata metadata.json --force
secretui import --metadata metadata.json          # build and display a plan
secretui import --metadata metadata.json --apply  # preflight and apply that plan
secretui import --metadata metadata.json --apply \
  --recovery recovery.json --report report.json
secretui completions bash > secretui.bash
secretui man > secretui.1
```

Export creates deterministic version 2 JSON without requesting secrets. By
default it refuses existing paths and symlink targets. `--force` explicitly
replaces an existing non-directory entry; replacing a symlink replaces the link
itself and never follows its target. Files created on Unix use mode `0600`.

Import is not a secret restore or a general migration facility. It matches only
exact collection and item object paths in the **same provider database**, checks
identity and current state, and changes collection labels, item labels, and item
attributes only. A conflict blocks all writes. `--apply` displays one plan,
creates a reusable version 2 recovery file and an operation report, repeats the
full preflight, then consumes that exact plan. By default those files are
created beside the input with a UTC timestamp; the flags above select explicit
no-clobber paths. A failed or partial application exits nonzero, and the report
is updated after every attempted operation. A provider can write successfully
and then fail verification, so any failed write attempt is reported as partial
rather than claiming that zero fields changed.

Labels and attributes can contain sensitive usernames, account identifiers,
service names, and internal hosts. Treat exports, plans, reports, and recovery
files as sensitive even though they contain no secret bytes. See
[METADATA.md](METADATA.md) for the complete format and limits.

Terminal output preserves normal Unicode but escapes backslashes, C0/C1 and ESC
controls, newlines/tabs/carriage returns, and bidirectional formatting controls.
Displayed labels are bounded to 256 graphemes, paths to 512, attribute keys and
values to 256/512, errors to 1,024, and attribute rows to 256. A truncated value
states its original UTF-8 byte length and a short SHA-256 identifier; storage and
matching always use the unmodified value.

## Secret and lock handling

Secrets are hidden by default. Reveal, Copy, Save, Delete, `import --apply`, and
any provider prompt are explicit authorization to perform the action and, when
needed, temporarily unlock its scope. SecretUI records the original collection
and item lock state, attempts to restore it after success or ordinary failure,
verifies relocking, and retries pending cleanup during normal shutdown.
Intentional Lock and Unlock actions remain persistent.

A crash, `SIGKILL`, machine failure, or forced terminal close can prevent
asynchronous relocking and clipboard cleanup. If SecretUI exits abnormally,
inspect and relock the affected collection with a trusted provider tool.

Details show a MIME-aware escaped UTF-8 or hexadecimal preview, never raw
terminal control sequences. Previews are limited to 256 secret bytes and expire
after 30 seconds. Owned secret buffers are zeroized when discarded. Copy Text
requires UTF-8; Copy Base64 uses padded RFC 4648 encoding and Copy Hex uses
compact lowercase hex. Clipboard clearing after 30 seconds is best-effort
because the desktop clipboard is outside this process.

## TUI keys

Pages flow `Collections → Items → Details`. `↑/↓` or `j/k` move,
`Enter`/`→`/`l`/`Tab` go forward, `Esc`/`←`/`h` go back, `/` searches inline,
and `?` shows help. On Collections or Items, `n` creates an object. Details
initially focuses **Back**; press `r` for an explicit reveal. Destructive dialogs
place and initially select **Cancel** before **Delete**.

Create and edit forms are scrollable in-TUI forms. Item metadata and secret
replacement are separate actions: metadata fields are applied only when changed
and secret replacement is never silently rolled back.

## Troubleshooting

### No session bus

Run SecretUI as the logged-in desktop user. `DBUS_SESSION_BUS_ADDRESS` should
usually identify that user's bus. `sudo`, a bare TTY, cron, and many containers
do not inherit it; do not copy another user's bus address into an untrusted
process.

### No provider or provider unavailable

Confirm that a Secret Service provider is installed, enabled, and running in
the same user session. Desktop-specific setup is outside SecretUI. A provider
can own `org.freedesktop.secrets` yet still reject an operation; SecretUI reports
the failing operation and sanitized target context.

### Unlock prompt dismissed or not visible

Retry only after checking the provider's graphical prompt. Over SSH or on a
headless host, the provider may be unable to display it. Dismissing a prompt is
treated as cancellation, not as an unlocked object.

### Clipboard unavailable

Clipboard access needs a usable graphical clipboard for the process. Remote
clipboard actions target the remote session, not automatically the local SSH
client. SecretUI intentionally has no secret-export or secret-backup command.

### Terminal looks damaged after interruption

Run `reset` or `stty sane`, then inspect the relevant provider lock state.
SecretUI escapes controls and restores terminal mode on ordinary shutdown, but
a forced termination can interrupt cleanup.

## Development

Use project-local Cargo/Rustup state if desired:

```bash
export CARGO_HOME=$PWD/.cargo-home
export RUSTUP_HOME=$PWD/.rustup
export CARGO_TARGET_DIR=$PWD/target
cargo fmt --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets
RUSTDOCFLAGS='-D warnings' cargo doc --locked --no-deps
```

The ignored live provider test creates globally unique temporary objects and is
never part of the default test run:

```bash
SECRETUI_INTEGRATION=1 \
  cargo test --locked --test integration_secret_service -- --ignored --nocapture
```

Never run it against important credentials without first reviewing the test.
See [RELEASE.md](RELEASE.md) for the reproducible release gates and
[CHANGELOG.md](CHANGELOG.md) for release notes.
