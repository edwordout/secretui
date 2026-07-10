**Objective:** Build an interactive TUI/CLI for browsing and managing Freedesktop Secret Service secrets safely.

**Key Results:**

1. Provide keyboard-driven navigation of collections, items, labels, object paths, and attributes.
2. Support non-secret-safe views by default, with secrets hidden unless explicitly revealed.
3. Add actions for reveal, copy, edit label/attributes, create item, delete item, lock/unlock collection, and search/filter.
4. Require confirmation for destructive actions and auto-clear copied/revealed secrets after a timeout.
5. Support KDE `ksecretd`, GNOME Keyring, and KeePassXC Secret Service backends.
6. Include export/import of metadata only, with optional encrypted secret backup as a separate guarded feature.
7. Ship as a single Linux binary with man page, shell completion, and examples.

A good product framing would be:

```text
secretui
```

or:

```text
secret-navigator
```

Core UX:

```text
secretui
```

opens an interactive terminal app where you can move through:

```text
Collections → Items → Details → Actions
```

with shortcuts like:

```text
/  search
r  reveal secret
c  copy secret
e  edit metadata
n  new secret
d  delete
l  lock/unlock
q  quit
```

This is probably more valuable than a GUI at first because it solves the exact pain point: `busctl` is powerful but ugly, and `secret-tool` is simple but too blunt.
