# Releasing SecretUI

This is the reproducible procedure for a SecretUI `0.1.x` patch release. It
builds from the tagged commit with the checked-in lockfile and Rust toolchain,
validates the user-facing artifacts, and verifies the files GitHub published.
It does not claim bit-for-bit reproducibility across arbitrary toolchains,
filesystems, or archive implementations.

## Release contract

- The package, lockfile, changelog, README examples, binary, annotated tag, and
  release title must use the same version.
- `main` CI must succeed before tagging.
- The tag must point to the tested `main` commit. GitHub Actions, not a
  maintainer workstation, builds the distributed binary from that tag.
- The x86-64 binary is built on `ubuntu-22.04`, is dynamically linked, and is
  documented as requiring glibc 2.34 or newer.
- Release notes come from that version's section in `CHANGELOG.md`.
- The release contains the binary, checksum, man page, Bash completion, image,
  README, security/changelog/metadata/release documents, and both licenses.
- No compatibility claim is added until the isolated live test passes against
  that exact source. The v0.1.2 gate is Ubuntu 26.04/KDE with KWallet 6.24.0;
  GNOME Keyring and KeePassXC remain unverified.

## One-time repository setup

1. Enable GitHub private vulnerability reporting under **Settings → Security →
   Code security and analysis**. Publication is blocked until this is enabled.
2. Protect `main` and require the CI `quality` job.
3. Allow GitHub Actions to create releases. The release workflow grants write
   permission only to its publish job.
4. Keep Actions pinned to full commit hashes.

No signing key is configured for v0.1.2, so use an annotated **unsigned** tag.
Do not substitute a lightweight tag. A future release may add a documented
signing identity.

## Prepare the source

Start from a clean checkout of `main`:

```bash
git switch main
git pull --ff-only origin main
test -z "$(git status --porcelain)"
```

For v0.1.2, confirm version consistency:

```bash
VERSION=0.1.2
TAG="v$VERSION"
test "$({ cargo metadata --locked --no-deps --format-version 1; } \
  | jq -r '.packages[] | select(.name == "secretui") | .version')" = "$VERSION"
grep -Fq "## [$VERSION]" CHANGELOG.md
grep -Fq "VERSION=$TAG" README.md
```

Review `git diff --check`, the release contract in the README, `SECURITY.md`,
provider status, and the complete changelog section. Confirm that documentation
does not describe metadata as a secret backup or general migration mechanism.

## Local release gates

Use the pinned Rust 1.97.0 toolchain and locked dependency graph:

```bash
export CARGO_HOME=${CARGO_HOME:-$PWD/.cargo-home}
export RUSTUP_HOME=${RUSTUP_HOME:-$PWD/.rustup}
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$PWD/target}

rustup show
cargo fmt --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets
RUSTDOCFLAGS='-D warnings' cargo doc --locked --no-deps
cargo audit
cargo build --locked --release --target x86_64-unknown-linux-gnu
```

Smoke-test generated interfaces and the release binary:

```bash
BIN=target/x86_64-unknown-linux-gnu/release/secretui
test "$($BIN --version)" = "secretui 0.1.2"
$BIN --help >/dev/null
$BIN export --help >/dev/null
$BIN import --help >/dev/null
$BIN man >/tmp/secretui.1
$BIN completions bash >/tmp/secretui.bash
test -s /tmp/secretui.1
test -s /tmp/secretui.bash
```

Verify a clean Cargo install without changing the workstation's normal Cargo
root:

```bash
install_root=$(mktemp -d)
CARGO_TARGET_DIR="$install_root/target" \
  cargo install --path . --locked --root "$install_root/root"
test "$("$install_root/root/bin/secretui" --version)" = "secretui 0.1.2"
rm -rf "$install_root"
```

Run the isolated provider gate in the available noncritical test wallet. Read
the test first and do not proceed if it can identify a pre-existing object:

```bash
SECRETUI_INTEGRATION=1 \
  cargo test --locked --test integration_secret_service -- --ignored --nocapture
```

The live test must use a globally unique collection and item, preserve binary
secret bytes/content type and metadata, and print sanitized manual cleanup
instructions on failure. Record the distribution, desktop, and provider version
in the release checklist; do not include wallet data in the record. The
publication script requires an explicit acknowledgement of the exact Ubuntu
26.04/KDE/KWallet 6.24.0 session before it runs this gate, so a passing test on a
different provider cannot silently authorize the compatibility claim.

## Validate the package shape

The tag workflow repeats formatting, Clippy, tests, warning-free docs, audit,
and a locked release build on Ubuntu 22.04. It then generates the man page and
Bash completion from the just-built binary. Its expected archive contents are:

```text
secretui-v0.1.2-x86_64-unknown-linux-gnu/
secretui-v0.1.2-x86_64-unknown-linux-gnu/secretui
secretui-v0.1.2-x86_64-unknown-linux-gnu/secretui.1
secretui-v0.1.2-x86_64-unknown-linux-gnu/secretui.bash
secretui-v0.1.2-x86_64-unknown-linux-gnu/ss.png
secretui-v0.1.2-x86_64-unknown-linux-gnu/README.md
secretui-v0.1.2-x86_64-unknown-linux-gnu/SECURITY.md
secretui-v0.1.2-x86_64-unknown-linux-gnu/CHANGELOG.md
secretui-v0.1.2-x86_64-unknown-linux-gnu/METADATA.md
secretui-v0.1.2-x86_64-unknown-linux-gnu/RELEASE.md
secretui-v0.1.2-x86_64-unknown-linux-gnu/LICENSE-APACHE
secretui-v0.1.2-x86_64-unknown-linux-gnu/LICENSE-MIT
```

The workflow extracts the archive, compares the exact sorted member list,
checks modes and nonempty documentation, executes `secretui --version`, and
validates `SHA256SUMS` before uploading either asset.

## Publish and independently verify

`/tmp/release-secretui-v0.1.2.sh` automates the maintainer-controlled portion.
Review it before execution. It deliberately stops for confirmation that private
vulnerability reporting is enabled, repeats the local gates, commits and pushes
`main`, and creates and pushes annotated tag `v0.1.2`. When the GitHub CLI (`gh`)
is installed and authenticated, it also verifies the repository security
setting, waits for public CI and the Release workflow, downloads the public
assets into a fresh directory, and repeats checksum/archive/version validation.
Without `gh`, it asks for explicit confirmation of the security setting, relies
on the tag-triggered workflow to repeat all release gates, and prints the Actions
and release URLs for manual monitoring and verification.

The workflow creates a draft release using the exact v0.1.2 changelog section,
uploads the archive and `SHA256SUMS`, then publishes the draft. If it fails after
draft creation, inspect and delete the draft before retrying; never move or
overwrite a published version tag.

After the script succeeds:

1. Open the public release in a logged-out browser.
2. Confirm the tag is annotated and targets the expected commit.
3. Confirm both assets download and the changelog-derived notes are complete.
4. Re-run the README install commands in a clean temporary root or VM.
5. Keep the release immutable. Fix a bad release with a new patch version.
