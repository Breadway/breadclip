# breadclip

A Wayland clipboard history manager for Hyprland. It consists of two binaries:

- **`breadclipd`** — a background daemon that watches the clipboard and persists entries to a local SQLite database.
- **`breadclip`** — a GTK4 Layer Shell popup for browsing and recalling clipboard history.

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
| `Escape` | Close without copying |

Clicking an entry copies it and closes the popup. Clicking outside the panel closes it.

### Filtering and search

The popup has three filter chips — **All**, **Text**, **Images** — and a search box. The search box filters text entries by content; image entries only appear under the **Images** filter.

## Data storage

History is stored under `$XDG_DATA_HOME/breadclip/` (typically `~/.local/share/breadclip/`):

| Path | Contents |
|------|----------|
| `history.db` | SQLite database of all entries |
| `images/` | PNG files for image entries |

The daemon keeps at most 200 text entries and 50 image entries, trimming oldest entries automatically.

### Privacy

- `history.db` and every file under `images/` are created with `0600` permissions (owner read/write only), regardless of your umask.
- breadclipd **never persists clipboard content flagged as sensitive by a password manager**. If a clipboard offer advertises the `x-kde-passwordManagerHint` MIME type — the convention used by KeePassXC, Bitwarden, and other password managers to mark content they own — that copy is skipped entirely and never reaches the database.
- That said, this is still a plaintext SQLite database of everything else you copy. Anything copied by an app that doesn't set the hint (e.g. copying a password from a terminal or a non-integrated app) will be stored like any other text entry. Treat `history.db` as sensitive, and don't rely on it as your only safeguard.

## Theming

`breadclip` inherits its colour palette from `bread-theme`. The panel renders with an 80% opaque background so Hyprland's `layerrule = blur` can show a frosted-glass effect behind it.
