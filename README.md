# cgui — visual front end for Apple's `container`

A fast, single-binary Rust TUI for [`apple/container`](https://github.com/apple/container)
built on [ratatui](https://ratatui.rs) + [crossterm](https://github.com/crossterm-rs/crossterm),
with a **Docker-compatible command shim** so muscle memory keeps working.

```
┌ cgui · Apple container front end ──────────────────────────────────────┐
│ [Containers]  Images  Volumes  Networks  Logs                          │
├──────────────────────┬─────────────────────────────────────────────────┤
│ CPU   12.4%          │ MEM    37.1% of limit                           │
│ ▁▂▃▅▇▆▅▃▂▁▁▂▃▅▇█▇▅▃ │ ▂▂▃▃▄▄▅▅▅▆▆▇▇▇▇▆▅▄▃▂                            │
├──────────────────────┴─────────────────────────────────────────────────┤
│ ID            IMAGE                            STATUS  CPUS  MEM       │
│▶ clerk-pg-test docker.io/pgvector/pgvector:pg16 running   4   1.0 GiB  │
│  redis-test    docker.io/library/redis:7        stopped   2  512 MiB  │
└────────────────────────────────────────────────────────────────────────┘
 q quit · ←→ tabs · ↑↓ select · r refresh · s start · x stop · K kill · d delete · l logs · a all
```

## Why

Apple's `container` is great but CLI-only. `cgui` gives you:

- **Live overview** — every running container, image, volume, and network at a glance, with sparklines for aggregate CPU/memory pulled from `container stats`.
- **One-key actions** — start, stop, kill, delete, view logs without leaving the TUI.
- **Drop-in `docker` muscle memory** — `cgui ps`, `cgui images`, `cgui rm`, `cgui rmi`, `cgui pull`, etc. translate to the right `container` invocation.

## Install

Requires:
- Rust 1.85+ (or update the pinned versions in `Cargo.toml`)
- Apple's `container` CLI on `$PATH` (and `container system start` running)

```bash
cargo build --release
cp target/release/cgui /usr/local/bin/   # optional
```

## Use

### TUI

```bash
cgui            # launch TUI
cgui tui        # same
```

| Key            | Action                                          |
| -------------- | ----------------------------------------------- |
| `q` / `Esc`    | Quit (or clear filter, if one is set)           |
| `Tab` / `→`    | Next tab                                        |
| `Shift+Tab`/`←`| Prev tab                                        |
| `↑` / `↓` / `j`| Move selection                                  |
| `Space`        | **Mark / unmark** the highlighted container for batch ops |
| `Enter`        | **Inspect** — open syntax-highlighted JSON detail pane |
| `/`            | **Filter** rows in current tab (Enter to apply) |
| `o`            | Cycle **sort** key for current tab              |
| `r`            | Refresh now                                     |
| `a`            | Toggle show-all vs running-only                 |
| `s`            | Start (operates on marked set, else highlighted row) |
| `x`            | Stop  (operates on marked set, else highlighted row) |
| `K`            | Kill  (operates on marked set, else highlighted row) |
| `d`            | Delete (operates on marked set, else highlighted row; clears marks on success) |
| `l`            | Load logs for selected → Logs tab               |
| `e`            | **Exec** — drop into `/bin/sh` in selected container (Ctrl-D returns to TUI) |
| `p`            | **Pull** an image (Images tab) — opens prompt + live progress modal |
| `P`            | **Re-attach** to a backgrounded pull (running or recently-finished) |
| `?`            | Toggle the **per-tab help** overlay                |
| **Mouse**      | Click a tab title to switch tabs · click a row to select it |

On the Logs tab `/` enters **search-as-you-type**: matches highlight in yellow as you type, with a live match counter in the title (`Logs · foo · search:err  (4 matches)`). Enter exits the input but keeps the highlight; `Esc` clears.

The pull modal renders a colored **Gauge** driven by a permissive parser of the streamed output (recognises `42%`, `12.3MB/45.6MB`, and `3/8` layer ratios — newest match wins). `Esc` backgrounds the modal: a yellow `⟳ pulling ref 42% — P to view` chip appears in the status bar so you can re-open it any time. When the pull finishes the chip turns green; pressing `P` shows the final log.

The Containers table shows **live CPU% and MEM** (used / limit) per row when a stats sample is available, with traffic-light coloring. Marked rows display a yellow `●` in the leftmost column.

On the **Volumes tab**, `Enter` opens a richer detail pane: capacity from the CLI, actual on-disk size from the backing image (sparse images are honest about it), a unicode fill bar (`[████░░░░░░] 42.3%`), and the full inspect JSON below.

User preferences (last tab, per-tab sort key, show-all toggle) are persisted to `$XDG_CONFIG_HOME/cgui/state.json` (defaults to `~/.config/cgui/state.json`). Saved on every relevant change and on quit; missing or malformed files are silently ignored.

In the Detail pane: `↑↓`/`PgUp`/`PgDn` scroll, `Esc` closes.
In the Pull modal: `Esc` hides; pull keeps running in the background and the status bar reports completion.

State refreshes every 2s; sparklines smooth across ~4 minutes of history (120 samples).

### Docker-compat shim

| You type                | Runs                          |
| ----------------------- | ----------------------------- |
| `cgui ps [-a]`          | `container ls [-a]`           |
| `cgui images`           | `container image ls`          |
| `cgui rmi REF`          | `container image delete REF`  |
| `cgui pull REF`         | `container image pull REF`    |
| `cgui push REF`         | `container image push REF`    |
| `cgui tag SRC DST`      | `container image tag SRC DST` |
| `cgui login REGISTRY`   | `container registry login …`  |
| `cgui logout REGISTRY`  | `container registry logout …` |
| `cgui rm ID`            | `container delete ID`         |
| `cgui top`              | `container stats`             |
| `cgui run …`            | `container run …` (passthrough)|
| `cgui exec …`           | `container exec …` (passthrough)|
| `cgui logs …`           | `container logs …` (passthrough)|
| `cgui build …`          | `container build …` (passthrough)|

Anything not in the table is passed through unchanged, so the shim never gets in your way.

## Architecture

- `src/container.rs` — async wrapper around the `container` binary; always invokes `--format json` and decodes defensively into typed structs.
- `src/cli.rs` — `clap`-based Docker-compat verb translator.
- `src/app.rs` — pure TUI state machine (no I/O in render path).
- `src/ui.rs` — ratatui rendering: tabs, tables, sparklines, status bar.
- `src/main.rs` — terminal setup, input + tick loop on `tokio::select!`.

State refresh is async and best-effort: if one source (e.g. `volume ls`) fails, the rest still update and the error surfaces in the status bar.

## Progress

| Feature                                              | Status     | Landed in       |
| ---------------------------------------------------- | ---------- | --------------- |
| Tabs · Containers/Images/Volumes/Networks/Logs       | ✅ shipped | 0.1.0           |
| Aggregate CPU/MEM sparklines                         | ✅ shipped | 0.1.0           |
| One-key lifecycle (start/stop/kill/delete/logs)      | ✅ shipped | 0.1.0           |
| Docker-compat CLI shim (`ps`, `images`, `rmi`, …)    | ✅ shipped | 0.1.0           |
| `e` exec shell-out (`/bin/sh` in selected container) | ✅ shipped | 0.2.0           |
| `p` image pull with live streaming progress modal    | ✅ shipped | 0.2.0           |
| `/` filter + `o` sort across all resource tabs       | ✅ shipped | 0.2.0           |
| `Enter` inspect detail pane (`container inspect` JSON)| ✅ shipped | 0.2.0           |
| Per-row live CPU/MEM in Containers table             | ✅ shipped | 0.3.0           |
| `Space` multi-select + batch start/stop/kill/delete  | ✅ shipped | 0.3.0           |
| Syntax-highlighted JSON in inspect pane              | ✅ shipped | 0.3.0           |
| Parsed % gauge for image pulls                       | ✅ shipped | 0.4.0           |
| Search-as-you-type in Logs tab (highlighted matches) | ✅ shipped | 0.4.0           |
| `Esc` backgrounds pull modal · `P` re-attaches       | ✅ shipped | 0.4.0           |
| Volume detail: capacity + on-disk usage + fill gauge | ✅ shipped | 0.5.0           |
| Per-tab help overlay (`?`)                           | ✅ shipped | 0.5.0           |
| Mouse: click tabs and rows to select                 | ✅ shipped | 0.5.0           |
| Persisted prefs (tab, sort, show-all) at `~/.config/cgui/state.json` | ✅ shipped | 0.5.0 |
| Optional GUI front end (Tauri)                       | 🟡 planned | —               |

## Roadmap

- Optional GUI front end (Tauri) sharing the same `container.rs` core
- Wheel scroll support in long views (logs, inspect, pull stream)
- Right-click context menu (start/stop/inspect quick actions)
- Configurable color theme via `~/.config/cgui/theme.toml`
- Image build progress modal (parallel of pull modal)
