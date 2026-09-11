# breadclip

A Wayland clipboard history manager for Hyprland. It consists of two binaries:

- **`breadclipd`**: a background daemon that watches the clipboard and persists entries to a local SQLite database.
- **`breadclip`**: a GTK4 Layer Shell popup for browsing and recalling clipboard history.

## Requirements

- Rust toolchain (edition 2021)
- GTK 4.12+
- `gtk4-layer-shell`
- `wl-clipboard` (`wl-paste` and `wl-copy` must be on `$PATH`)
- Hyprland (or any Wayland compositor with Layer Shell support, though the panel-positioning logic is Hyprland-specific)

## Build

```sh
git clone https://git.breadway.dev/breadway/breadclip
cd breadclip
cargo build --release
```

The compiled binaries are at `target/release/breadclip` and `target/release/breadclipd`.

## Install

Copy the binaries to somewhere on your `$PATH`, e.g.:

```sh
cp target/release/breadclip target/release/breadclipd ~/.local/bin/
```

### systemd user service

A unit file is provided in `contrib/`:

```sh
cp contrib/breadclipd.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now breadclipd
```

### Hyprland keybind

Add the contents of `contrib/hyprland.conf` to your `hyprland.conf`:

```
layerrule = blur, breadclip
layerrule = ignorezero, breadclip

bind = $mainMod, V, exec, breadclip
```

The `layerrule` lines enable the frosted-glass blur effect behind the panel.

## Usage

Start the daemon (or let the systemd unit handle it):

```sh
breadclipd
```

Open the clipboard history popup:

```sh
breadclip
```

Running `breadclip` a second time while it is open closes it (toggle behaviour).

### Keyboard shortcuts

| Key | Action |
|-----|--------|
| `Up` / `Down` | Move selection |
| `Enter` | Copy selected entry to clipboard and close |
| `Delete` | Remove selected entry from history |
| `Ctrl+P` | Pin/unpin selected entry (pinned entries survive trimming and sort to the top) |
| `Escape` | Close without copying |

Clicking an entry copies it and closes the popup. Clicking outside the panel closes it.

### Filtering and search

The popup has three filter chips (**All**, **Text**, **Images**) and a search box. The search box filters text entries by content; image entries only appear under the **Images** filter.

## Configuration

Optional TOML config at `$XDG_CONFIG_HOME/breadclip/config.toml` (typically
`~/.config/breadclip/config.toml`). Every key has a sensible default, so the
file can be omitted entirely; a copy of the full example lives in
`contrib/config.toml.example`:

```toml
[retention]
text = 200    # max non-pinned text entries (0 = keep none)
images = 50   # max non-pinned image entries (0 = keep none)

[panel]
width = 520   # popup panel width, px

[capture]
primary = false  # also watch the middle-click primary selection

[ignore]
patterns = []    # extra regex patterns to treat as sensitive
disable = []     # built-in secret rules to switch off, by name
```

## Data storage

History is stored under `$XDG_DATA_HOME/breadclip/` (typically `~/.local/share/breadclip/`):

| Path | Contents |
|------|----------|
| `history.db` | SQLite database of all entries |
| `images/` | PNG/JPEG files for image entries |

The daemon trims the oldest non-pinned entries automatically, keeping at
most `retention.text` text entries and `retention.images` image entries
(defaults 200 and 50; configurable). **Pinned entries are exempt from
trimming** and sort to the top of the popup. Entries captured from the
primary (middle-click) selection (when `capture.primary = true`) are
stored alongside regular clipboard entries with a `primary` badge.

### Privacy

- `history.db` and every file under `images/` are created with `0600` permissions (owner read/write only), regardless of your umask.
- breadclipd **never persists clipboard content flagged as sensitive**. `wl-paste --watch` reports copies made with `wl-copy --sensitive` (which also covers offers advertising the `x-kde-passwordManagerHint` MIME type, the convention used by KeePassXC, Bitwarden, and other password managers to mark content they own) via `CLIPBOARD_STATE=sensitive`, and those copies are skipped entirely and never reach the database.
- On top of that, breadclipd runs **best-effort ignore rules** that skip copies that *look* like secrets even when the app didn't flag them: one-time codes, Luhn-valid credit card numbers, private key blocks, `password:`-style credential lines, and well-known API token prefixes (see `breadclipd/src/ignore_rules.rs`). These are deliberately conservative and are a convenience, not a security boundary: the 0600/0700 permissions are the real protection. The `[ignore]` section of `config.toml` tunes them: `patterns` adds extra regexes to treat as sensitive, and `disable` switches off a built-in rule by name (`private_key`, `credential_line`, `otp`, `card_number`, `api_token`) when it keeps tripping on something you want kept.
- That said, this is still a plaintext SQLite database of everything else you copy. Anything copied by an app that doesn't set the hint (e.g. copying a password from a terminal or a non-integrated app) will be stored like any other text entry. Treat `history.db` as sensitive, and don't rely on it as your only safeguard.

## Theming

`breadclip` inherits its colour palette from `bread-theme`. The panel renders with an 80% opaque background so Hyprland's `layerrule = blur` can show a frosted-glass effect behind it.
