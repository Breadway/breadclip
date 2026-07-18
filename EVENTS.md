# breadclip — bread event integration

breadclip is a standalone clipboard manager: it works exactly the same with
or without `breadd` running. When breadd *is* present, `breadclipd` publishes
events into the shared bread automation fabric and listens for a small set
of commands. See the parent `bread` repo's `Documentation.md` — specifically
its "Namespaces" and "Integrating a bread\* app" sections — for the general
convention this follows.

App id: **`clip`**. Transport: `bread-utils`'s `bread_client` module
(feature `bread-client`) — `breadclipd` links it directly rather than
shelling out, since it's a long-running process for the command-subscription
half (though each `emit` call is still its own short-lived connection, since
`capture_once()` — the actual clipboard-read code — runs as a fresh
per-clipboard-change process invocation, not inside a persistent loop).

## Events published (`bread.clip.*`)

| Event | Data | When |
|-------|------|------|
| `bread.clip.copied` | `{ "kind": "url" \| "error" \| "code" \| "path" \| "plain", "len": <bytes or chars> }` | Every successful clipboard capture (text or image — images always get `kind: "image"`). `kind` is a heuristic classification (see `breadclipd/src/content_kind.rs`), not a guarantee — don't build a security decision on it. |
| `bread.clip.clear.done` | `{}` | `bread.command.clip.clear` was received and history was successfully cleared. |
| `bread.clip.clear.failed` | `{ "error": "<message>" }` | `bread.command.clip.clear` was received but clearing failed (e.g. DB error). |

Content is never included in the payload — only its detected kind and length. History (including the actual copied content) stays local to breadclip's own SQLite database; the event bus is for *notifications about* clipboard activity, not a channel for clipboard content itself.

## Commands honored (`bread.command.clip.*`)

| Verb | Effect |
|------|--------|
| `clear` | Deletes all clipboard history (text entries and stored image files). Emits `bread.clip.clear.done`/`.failed`. |

### Not implemented: `pin` / `select`

An earlier draft of this integration planned `pin`/`select` verbs, but
breadclip's history schema has no "pinned" concept at all today — there's no
column for it, and the GTK popup UI has no corresponding affordance. Adding
real pin/select support is a product decision for breadclip itself (does it
want pinning, and what should the UI look like?), not something to fabricate
as a side effect of wiring up the event bus. If/when breadclip grows that
feature, the corresponding `bread.command.clip.pin`/`.select` verbs (and
matching `bread.clip.pinned`/`.selected` events) should be added at the same
time, not stubbed out ahead of it.

## Fail-safe behavior

- If breadd isn't installed or isn't running, `emit` is a silent no-op
  (`BreadClient::emit` never blocks or errors the caller) and the
  command subscription simply never receives anything — breadclip's actual
  clipboard-history functionality is entirely unaffected either way.
- If breadd restarts, the command subscription reconnects automatically
  (`BreadClient::subscribe`'s background thread has its own backoff loop);
  no restart of breadclipd is needed.
