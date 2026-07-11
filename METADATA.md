# SecretUI metadata format

This document defines metadata JSON accepted and emitted by SecretUI v0.1.2.
Metadata contains **no secret values** and is not a secret backup. It is intended
for inspection and exact-path, same-store metadata repair.

## Security classification

Treat every metadata, recovery, and apply-report file as sensitive. Labels,
attributes, timestamps, and provider object paths can disclose usernames,
accounts, applications, internal hostnames, and usage patterns even though the
secret bytes are absent.

SecretUI never retrieves a secret while browsing or exporting metadata. Version
2 does not represent secret content type either. Do not publish a real export in
an issue or vulnerability report; construct a minimal document with synthetic
values instead.

## Version 2

Version 2 is the current export and recovery format. All fields below are
required except `created` and `modified`, which may be omitted or `null`.

```json
{
  "version": 2,
  "collections": [
    {
      "path": "/org/freedesktop/secrets/collection/example",
      "label": "Example collection",
      "locked": false,
      "items": [
        {
          "path": "/org/freedesktop/secrets/collection/example/1",
          "label": "Example item",
          "locked": false,
          "attributes": {
            "application": "example",
            "username": "alice"
          },
          "created": 1783785600,
          "modified": null
        }
      ]
    }
  ]
}
```

### Root object

| Field | Type | Meaning |
| --- | --- | --- |
| `version` | integer | Exactly `2`. |
| `collections` | array | Collection metadata objects. |

### Collection object

| Field | Type | Meaning |
| --- | --- | --- |
| `path` | string | Exact provider D-Bus collection object path. |
| `label` | string | Collection label. |
| `locked` | boolean | Observed provider lock state. Used for preflight, not applied. |
| `items` | array | Items observed in this collection. |

### Item object

| Field | Type | Meaning |
| --- | --- | --- |
| `path` | string | Exact provider D-Bus item object path. |
| `label` | string | Item label. |
| `locked` | boolean | Observed provider lock state. Used for preflight, not applied. |
| `attributes` | object | Complete string-to-string Secret Service attribute map. |
| `created` | unsigned integer or `null` | Provider-reported creation time in Unix seconds, when available. |
| `modified` | unsigned integer or `null` | Provider-reported modification time in Unix seconds, when available. |

`created` and `modified` are provider observations, not portable or universally
meaningful timestamps. Repair never writes them. A meaningful `modified` value
is included in concurrent-change checks.

Version 2 has no `content_type` field. Secret content type is available through
a secret-retrieval API, so metadata-only export deliberately does not request or
represent it.

## Version 1 compatibility

Version 1 has the same shape as version 2 and additionally permits the legacy
item field:

```json
"content_type": "text/plain"
```

The legacy field may be a string or `null` and may be absent. SecretUI accepts
and deliberately ignores it. It is never verified or applied, because doing so
would require secret access and version 1 exports cannot be trusted as evidence
of the current secret content type. Recovery and new exports always use version
2.

## Strict parsing and limits

Parsing is strict and happens before provider mutation. Unknown fields are
rejected at every object level. In particular, version 2 rejects
`content_type`. Unknown format versions are rejected and unknown fields are not
preserved for a later export.

The following hard limits apply to input:

| Resource | Limit |
| --- | ---: |
| JSON document | 16 MiB |
| Collections | 10,000 |
| Items, across the whole document | 100,000 |
| Attributes on one item | 1,024 |
| Label or object path | 16 KiB of UTF-8 |
| Attribute key | 4 KiB of UTF-8 |
| Attribute value | 64 KiB of UTF-8 |

Every path must be a syntactically valid D-Bus object path. Collection paths
must be unique. Item paths must be globally unique, not merely unique within one
collection. The item must actually be a child of the collection containing it;
nesting an existing item under another collection is a conflict.
Labels, attribute keys, and attribute values must also be representable as
D-Bus strings; an interior NUL is rejected before planning or mutation.

## Deterministic export

Export produces UTF-8, pretty-printed version 2 JSON followed by one newline.
Collections are ordered lexicographically by collection path, items by item
path, and attribute keys by key. Determinism applies for the same provider
metadata snapshot; provider-supplied values can naturally change between reads.

By default export refuses an existing destination, a destination symlink, or an
unsafe target. `--force` explicitly permits replacement of an existing
non-directory entry. If that entry is a symlink, SecretUI replaces the link
itself and never follows its target. On Unix, the resulting regular file has mode
`0600`. SecretUI writes and syncs a temporary file, installs the final file
atomically in the destination directory, and syncs that parent directory.

Directory permissions, filesystems without Unix mode semantics, network
filesystems, backups, and synchronization software remain outside SecretUI's
control.

## Same-store metadata repair

`secretui import --metadata FILE` creates and displays a deterministic plan in
collection-path then item-path order. Matching is based only on exact object
paths in the same provider database. An export is not a portable cross-provider
identity map: provider object paths may change when a database is recreated,
restored, or accessed through another implementation.

Planning checks, as applicable:

- unique and accessible collection and item identities;
- the item's actual parent collection;
- current collection/item labels and complete item attributes;
- collection and item lock state; and
- provider identity and modified timestamp.

Current and proposed values are rendered with terminal-safe escaping, but the
original unmodified strings remain the values used for comparison and mutation.
Any duplicate, missing/inaccessible object, parent mismatch, state drift, or
other conflict blocks all writes.

`--apply` builds and displays one plan, creates the recovery and report files,
performs a second full preflight, and applies that exact plan. It never reparses
the source to discover a different set of writes after confirmation. Collection
labels are applied before items; within an item the label precedes attributes.
No-op fields are skipped. The lock state, timestamps, paths, secret bytes, and
secret content type are never applied.

Application stops on the first operation or durable-report failure. There is no
partial-apply mode. A partial result and every relock failure are recorded and
cause a nonzero exit. Earlier provider writes might already be durable, so the
report and recovery file must be reviewed rather than assuming transaction-like
rollback.

## Recovery and apply reports

Before the first provider write, SecretUI creates two no-clobber files beside
the input by default:

```text
<input-name>.secretui-recovery-<UTC timestamp>.json
<input-name>.secretui-report-<UTC timestamp>.json
```

`--recovery PATH` and `--report PATH` select explicit destinations. Existing
paths and symlink targets are refused. Files use Unix mode `0600`.

The recovery document is deterministic version 2 metadata containing the
original values needed for affected targets. It can be passed directly to
`secretui import --metadata RECOVERY_FILE` to plan a repair back toward that
snapshot. It still contains no secret values and cannot recover a deletion or
secret replacement.

The apply report records collection and item changes separately, field
operations attempted/verified-applied/skipped, conflicts, failures, relock
failures, and whether application was complete, blocked, or partial. A provider
call can perform its write and then fail verification, so any attempted write
with an error is conservatively partial even when zero operations were verified.
The report is atomically updated and durably synced after every operation. An
apply report is an audit result, not a metadata input document.

See [`examples/metadata-only.json`](examples/metadata-only.json) for a synthetic
version 2 example.
