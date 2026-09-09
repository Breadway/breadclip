# breadclip: bread event integration

breadclip is a standalone clipboard manager: it works exactly the same with
or without `breadd` running. When breadd *is* present, `breadclipd` publishes
events into the shared bread automation fabric and listens for a small set
of commands. See the parent `bread` repo's `Documentation.md` (specifically
its "Namespaces" and "Integrating a bread\* app" sections) for the general
convention this follows.

App id: **`clip`**. Transport: `bread-utils`'s `bread_client` module
(feature `bread-client`): `breadclipd` links it directly rather than
shelling out, since it's a long-running process for the command-subscription
half (though each `emit` call is still its own short-lived connection, since
`capture_once()` (the actual clipboard-read code) runs as a fresh
per-clipboard-change process invocation, not inside a persistent loop).

## Events published (`bread.clip.*`)

| Event | Data | When |
|-------|------|------|
| `bread.clip.copied` | `{ "kind": "url" \| "error" \| "code" \| "path" \| "plain", "len": <bytes or chars> }` | Every successful clipboard capture (text or image; images always get `kind: "image"`). `kind` is a heuristic classification (see `breadclipd/src/content_kind.rs`), not a guarantee: don't build a security decision on it. |
| `bread.clip.clear.done` | `{}` | `bread.command.clip.clear` was received and history was successfully cleared. |
| `bread.clip.clear.failed` | `{ "error": "<message>" }` | `bread.command.clip.clear` was received but clearing failed (e.g. DB error). |
| `bread.clip.pinned` | `{ "id": <entry id>, "pinned": true \| false }` | An entry was pinned or unpinned: either via `bread.command.clip.pin`, or locally from the popup's Ctrl+P toggle (which updates the DB directly; see below). |
| `bread.clip.pin.failed` | `{ "error": "<message>" }` | `bread.command.clip.pin` was received but pinning failed (e.g. DB error, or the command was missing its `id`). |

Content is never included in the payload: only its detected kind and length. History (including the actual copied content) stays local to breadclip's own SQLite database; the event bus is for *notifications about* clipboard activity, not a channel for clipboard content itself.

## Commands honored (`bread.command.clip.*`)

| Verb | Effect |
|------|--------|
| `clear` | Deletes all clipboard history (text entries, stored image files, pinned entries; it's a hard reset). Emits `bread.clip.clear.done`/`.failed`. |
| `pin` | Payload `{ "id": <entry id>, "pin": true \| false }`. Pins or unpins a history entry by id; `pin` defaults to `true` if omitted. Emits `bread.clip.pinned`/`bread.clip.pin.failed`. |

Pinned entries are exempt from the daemon's trimming and sort to the top of
the history list. The popup's Ctrl+P toggle writes the same `pinned` column
directly (it doesn't round-trip through the bus), and also emits
`bread.clip.pinned`, so automation can observe either path. Entry `id`s come
from `list_entries`: there is currently no read API on the bus for the
history contents.

### Not implemented: `select`

`select` (remote-activate a row from the bus, e.g. "paste entry 42 now")
is still deliberately not implemented: the popup is a short-lived process
with no resident service to receive that command, and the daemon has no
reason to talk to a transient popup. That's a real product decision for
breadclip itself (what should "select from the bus" even mean when the
popup isn't open?), not something to fabricate as a side effect of wiring
up the event bus. When breadclip grows that feature, the matching
`bread.command.clip.select` verb and `bread.clip.selected` event should be
added at the same time.

## Fail-safe behavior

- If breadd isn't installed or isn't running, `emit` is a silent no-op
  (`BreadClient::emit` never blocks or errors the caller) and the
  command subscription simply never receives anything: breadclip's actual
  clipboard-history functionality is entirely unaffected either way.
- If breadd restarts, the command subscription reconnects automatically
  (`BreadClient::subscribe`'s background thread has its own backoff loop);
  no restart of breadclipd is needed.
