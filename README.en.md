# LobbyTracker

[Français](README.md) · **English**

> **Merged project: this repository is no longer updated.**
> LobbyTracker and [RL Session Stats](https://github.com/NeyorDEV/rl-session-stats) have been merged into **[RL Tracker Suite](https://github.com/NeyorDEV/rl-tracker-suite)**, a single app with a *Session Stats* tab and a *LobbyTracker* tab.
> This repository will not receive any further updates: development continues only in [RL Tracker Suite](https://github.com/NeyorDEV/rl-tracker-suite).
>
> - New project: [NeyorDEV/rl-tracker-suite](https://github.com/NeyorDEV/rl-tracker-suite)
> - Original projects: [NeyorDEV/lobbytracker](https://github.com/NeyorDEV/lobbytracker) (this repository) and [NeyorDEV/rl-session-stats](https://github.com/NeyorDEV/rl-session-stats)

**The passive pre-match radar for Rocket League.** Real rank, smurfs, 1v1 threats, players on a lose streak: know who you are facing before kickoff, without ever touching the game.

Tauri v2 · Rust · SQLite · transparent overlay · OBS browser source

---

## Why it is Easy Anti-Cheat friendly

LobbyTracker **never** interacts with the game process.

| Forbidden (and absent from the code) | What LobbyTracker does |
|---|---|
| DLL injection | Reads `Launch.log` read-only (standard file sharing) |
| `ReadProcessMemory` / `WriteProcessMemory` | Optionally listens to the **official Stats API** (TCP `127.0.0.1`) |
| DirectX / Vulkan hooks | Overlay = separate transparent Windows window, zero FPS cost |
| Requests inside the render loop | Network requests run on an async thread, outside the game, with a SQLite cache |

> No tool can promise zero risk, and there is no guarantee from Epic. This design simply avoids every technique anti-cheats target.

## Features

- **Lobby radar**: opponents are processed first, then teammates.
- **Instant display (< 50 ms)** for players already in the SQLite cache (WAL, 24 h TTL); the others resolve in the background with a discreet indicator.
- **Badges, all of them can be turned off** in the settings. An unchecked category is neither computed nor sent to the overlay:
  | Badge | Default rule |
  |---|---|
  | Potential smurf (*Potentiel smurf*) | < 250 total wins and rank ≥ Champion 2 (adjustable thresholds), or a young account climbing fast (≥ 15 recent ranked matches, ≥ 70 % wins, ≥ +150 MMR) + *Smurf Index* 0-100 |
  | 1v1 player (*Joueur 1v1*) | 1v1 rank ≥ lobby 2v2/3v3 median + 2 tiers (≥ 10 1v1 matches) |
  | Goalkeeper (*Gardien*) | ≥ 1.6 saves/match and saves ≥ 2.5 × goals |
  | Lose streak | ≥ 3 consecutive losses (shows the count) |
  | Former high rank (*Ancien haut rang*) | Peak ≥ Champion 2 and current rank ≥ 2 tiers below the peak |
  | Opponents only | Hides teammates |
- **Tauri overlay**: transparent, borderless, always on top, configurable global hotkeys (`F9` show/hide, `Ctrl+Shift+C` click-through), free positioning with a saved position or screen anchors with margins. Four layouts (vertical cards, horizontal cards, column, row), scale from 50 to 200 %, adjustable opacity, customisable colours, and automatic hiding when a lobby fills: your choice of either as soon as the 3-2-1 countdown ends (Stats API enabled, `RoundStarted` event) or after N seconds.
- **OBS**: add a *Browser Source* at `http://127.0.0.1:49125/obs` (transparent background, updated over WebSocket).
- **Hybrid scraper**: invisible WebView2 (passes Cloudflare like a real browser), then falls back to the official TRN API with your token.
- **Automatic account detection**: your account and platform (Epic, Steam…) are detected from `Launch.log` at each Rocket League launch, so switching accounts needs no manual setup.

## Architecture

```
Launch.log ──► tracker-watcher ─┐
Stats API  ──►                  ├─► tracker-parser ─► tracker-engine ─► tracker-heuristics ─► LobbySnapshot
                                │        (IDs)           │  ▲                                   │
                                │                        ▼  │                                   ├─► overlay (Tauri events)
                                │              tracker-cache (SQLite)                           └─► tracker-server (WS + OBS)
                                │                        ▲
                                └──────── tracker-scraper (priority queue, WebView2 / TRN API)
```

| Crate | Role |
|---|---|
| `tracker-core` | Shared types, configuration (`serde_json`), views |
| `tracker-watcher` | Async tail of `Launch.log` (detects truncation/recreation), Stats API client with reconnection |
| `tracker-parser` | Extracts Epic IDs (32 hex), SteamID64, Xbox, PSN; Stats API messages |
| `tracker-cache` | SQLite WAL: players, playlists, timestamps, TTL |
| `tracker-scraper` | WebView2 / TRN API sources, JSON mapping, priority queue |
| `tracker-heuristics` | Smurf Index, badges, filters, sorting |
| `tracker-server` | HTTP + WebSocket on `127.0.0.1` only |
| `tracker-gamecfg` | Epic / Steam install detection, `PacketSendRate` setting |
| `tracker-engine` | Orchestration and lobby state |
| `src-tauri` + `ui/` | Application, windows, hotkeys, interface |

### Cloudflare workaround

Tracker Network returns 403 to bare HTTP clients. LobbyTracker therefore opens the internal API URL in a **hidden WebView2 window**; once the challenge is passed, a script *from our own window* sends the displayed JSON back to the application (Tauri command `submit_scrape`, restricted to `api.tracker.gg`, with an HTTP fallback to `127.0.0.1`). Request identifiers are random and single-use.

> Use of Tracker Network data remains subject to their terms of service; you are responsible for it. The official API token is the cleanest option.

## Installation

Requirements: Windows 10/11 (WebView2 included), stable [Rust](https://rustup.rs), and the [Tauri v2 prerequisites](https://v2.tauri.app/start/prerequisites/).

```bash
cargo install tauri-cli --version "^2" --locked
cargo tauri dev          # development
cargo tauri build        # NSIS installer
```

For live teams (and therefore the opponent / teammate distinction), the game's Stats API must be enabled. In the settings, the **Stats API de Rocket League** section detects Epic Games and Steam installs and sets `PacketSendRate` in `DefaultStatsAPI.ini` (10 / 30 / 60 per second); a `.lobbytracker.bak` copy is kept. Restart the game after the first change; afterwards LobbyTracker restores the value if a game update erases it. Your account is detected automatically.

### Automatic updates

The app checks GitHub Releases for a newer version at startup and offers to install it; the signature is verified before installing. To publish updates, add `TAURI_SIGNING_PRIVATE_KEY` (and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`, empty if the key has no password) to the repository secrets, then publish (not draft) the release created by the workflow. Local builds sign with the same environment variable.

## Tests

```bash
cargo test --workspace
```

Tests cover the parser (ID formats, false positives, Stats API), heuristics and filters, the cache (TTL, WAL), the priority queue, file tailing, Tracker Network mapping and the local server.

## Known limitations (please read)

- **`Launch.log` format**: it is undocumented. The parser relies on the canonical `Platform|id|0` form and on the `BeaconJoin`, `LogNet`, `LoadMap` keywords. Adjust `crates/tracker-parser/src/lib.rs` if your log differs. Names containing spaces without a separator may be extracted incorrectly; the Stats API is the most reliable source.
- **Tracker Network schema**: checked against a real profile (rank per mode, peak tier, per-mode averages, `sessions` history), but not guaranteed to stay stable. Epic, Xbox and PSN accounts are looked up by name, so a player whose name is not known yet stays in "loading".
- **WebView2 scraper**: written and compiled, but not tested in real conditions against Cloudflare.
- Ports and paths require a restart; filters, token, hotkeys, click-through and the overlay's position and dimensions apply live.

## License

Released under the [MIT License](LICENSE): you may use, copy, modify and redistribute this code, including in commercial projects, as long as the copyright notice and the license text are kept. The software is provided "as is", without warranty.

Copyright (c) 2026 [NeyorDEV](https://github.com/NeyorDEV)

This project is now merged into [RL Tracker Suite](https://github.com/NeyorDEV/rl-tracker-suite), also released under the MIT License.
