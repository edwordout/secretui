# Security policy

## Supported versions

Only the latest `0.1.x` patch release receives security fixes. Older patches,
development snapshots, and locally modified builds are unsupported. Upgrade to
the newest published patch before reporting a problem that may already be fixed.

| Version | Supported |
| --- | --- |
| Latest `0.1.x` patch | Yes |
| Earlier `0.1.x` patches | No |
| Unreleased source snapshots | No |

## Report a vulnerability privately

Use [GitHub private vulnerability reporting](https://github.com/edwordout/secretui/security/advisories/new).
Do not open a public issue for a suspected vulnerability. Private vulnerability
reporting must be enabled in the repository before any release is published.

Include, when relevant:

- the SecretUI version and installation source;
- Linux distribution, desktop environment, terminal, and architecture;
- Secret Service provider and version;
- whether the collection and item were locked;
- minimal reproduction steps and the expected/observed behavior; and
- sanitized error text or a minimal synthetic metadata document.

**Do not attach or paste secret values, real metadata exports, recovery or apply
reports, wallet/database files, screenshots containing revealed secrets, raw
D-Bus captures, clipboard contents, core dumps, or full environment dumps.**
Replace labels, paths, attributes, usernames, hostnames, and account identifiers
with synthetic values while preserving the shape needed to reproduce the issue.

The maintainer aims to acknowledge a complete report within seven days. Timing
for validation and a fix depends on severity and provider access; this is a
best-effort project policy, not a service-level agreement. Please coordinate
public disclosure until a fix and advisory are available. After users have had
a reasonable upgrade window, the maintainer may publish a GitHub security
advisory with credit, impact, affected versions, and remediation unless the
reporter requests otherwise.

## Threat model

SecretUI is a human-operated client of an existing Secret Service provider. It
does not establish the provider's trust boundary and cannot make an already
compromised login session safe.

### Risk reductions

SecretUI is designed to reduce:

- accidental shoulder-surfing and persistent display by hiding secrets until an
  explicit action and expiring previews after 30 seconds;
- accidental terminal control execution by escaping control and bidirectional
  formatting characters in provider and file-supplied text;
- accidental clipboard retention by attempting a best-effort clear after 30
  seconds;
- secret disclosure through browsing, metadata export, recovery documents,
  apply reports, or operation logs by never retrieving or recording secret
  values for those workflows;
- routine memory retention by zeroizing SecretUI-owned secret buffers when they
  are discarded; and
- unintended persistent unlocks by remembering original lock state, relocking
  temporary unlocks, verifying cleanup, and retrying pending relocks on normal
  shutdown.

Metadata edits are separate from secret replacement. Same-store metadata repair
matches exact D-Bus object paths, performs a full conflict preflight before any
write, and creates secret-free recovery and durable report files.

### Out of scope

SecretUI does not protect against:

- root, another privileged process, or malicious code running as the same user;
- a compromised kernel, Secret Service provider, desktop session, D-Bus,
  terminal emulator, or dependency;
- clipboard managers, clipboard history, another clipboard reader, screen
  capture, screen recording, or observation of an unlocked session;
- swap, hibernation images, core/crash dumps, allocator-internal copies, kernel
  buffers, or copies retained by provider/clipboard/terminal software;
- physical access to a logged-in, unlocked session;
- malicious metadata intentionally revealing its own labels, attributes, or
  paths after those values have been rendered safely; or
- recovery of deleted secrets, encrypted secret backup, secret migration, or
  reliable operation across every provider implementation.

Zeroization narrows exposure of the buffers SecretUI owns; it is not proof that
every historical copy has disappeared. Clipboard clearing is similarly
best-effort and cannot erase a clipboard manager's history.

## Lock and process-lifecycle limitations

Reveal, Copy, Save, Delete, `import --apply`, and any provider authorization
prompt are deliberate authorization for the requested operation and any
temporary unlock it requires. Intentional Lock and Unlock commands are
persistent. Ordinary operation failures still trigger relock attempts, and
relock failures are reported separately from the operation result.

A crash, abort, `SIGKILL`, power loss, or provider/session failure can prevent
asynchronous cleanup. After abnormal termination, use a trusted provider tool to
inspect and relock the affected collection. The same limitation applies to
best-effort clipboard clearing and terminal restoration.

## Metadata classification

Metadata JSON never contains secret bytes or a secret content type in version 2,
but it is not public data. Labels, attributes, timestamps, and object paths can
identify people, services, internal hosts, accounts, and usage patterns. Export,
recovery, and report files are created with Unix mode `0600`; users remain
responsible for directory permissions, backups, synchronization tools, and file
retention. See [METADATA.md](METADATA.md).
