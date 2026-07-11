# secretui Intended UX

`secretui` should feel like a safe terminal file browser for secrets: simple pages, obvious movement, and conservative handling of secret values.

## Core Flow

Use navigable pages, not a three-pane layout:

```text
Collections → Items → Details
```

- **Collections page**: choose a keyring/wallet collection.
- **Items page**: choose a secret in that collection.
- **Details page**: view label, object path, attributes, lock state, and actions.
- Secret values are always shown as `<hidden>` unless explicitly revealed.

## Navigation

Navigation must work with standard terminal keys and have visible hints on every page.

- `↑/↓`: move selection on list pages.
- `Enter` or `→`: go to the next page.
- `Esc` or `←`: go to the previous page / close overlays.
- On Collections, `n` creates a New Collection.
- On Items, `n` creates a New Item.
- On Details, `↑/↓` scrolls content, `←/→` selects an action button, and `Enter` activates it.
- `Tab`: next page fallback.
- `/`: search/filter items in-place.
- `?`: show help.
- `q`: quit.

Each page should have a clear title, selected row marker, breadcrumb/header, and footer hints.

## Actions

Actions are explicit buttons and forms inside the TUI. The app should never drop into shell-style prompts for create, edit, or delete.

Buttons:

- `Reveal`: reveal selected secret temporarily and auto-scroll to the revealed line.
- `Copy`: copy selected secret temporarily.
- `Edit`: edit label/attributes/secret.
- `Delete`: delete selected item.
- `Lock/Unlock`: lock or unlock selected collection.
- `Back`: return to Items.

Create/edit/delete forms use scrollable bordered input widgets with visible focus, cursor editing, validation messages, arrow-first navigation, and Save/Delete/Cancel buttons. Secret fields are masked while typing. Item forms contain an attribute list ending with `+ Create new attribute`; Enter opens a focused key/value editor for Add, Update, or Remove and then returns to the list. Inputs and lists keep readable minimum heights; if the terminal is too short, elements are hidden instead of crushed and scrollbars show overflow.

Changed forms display an `(unsaved)` marker. Esc or Cancel opens an Unsaved Changes screen with Save Changes, Discard, and Keep Editing, defaulting to Keep Editing. Attribute editor Cancel returns to the parent draft without changing it.

Destructive or sensitive actions require confirmation. Reveal/copy should auto-expire after a short timeout.

## Safety Rules

- Never show secrets by default.
- Never log secret values.
- Zeroize only buffers owned by the app.
- Clipboard clearing is best-effort and should be described as such.
- Metadata export/import must not include secrets.
- Secret backup/restore remains unavailable until it can be scoped, previewed, encrypted without retaining unnecessary plaintext copies, and restored without unsafe collection fallbacks.

## Desired Feel

The app should feel closer to a simple wizard-like browser than a D-Bus inspector. Users should always know:

1. which page they are on,
2. what is selected,
3. how to move forward/back,
4. what actions are available,
5. whether a secret is hidden, revealed, or copied.
