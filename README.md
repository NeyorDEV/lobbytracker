# LobbyTracker

**Le radar d'avant-match passif pour Rocket League.** Rang réel, smurfs, menaces 1v1, démolisseurs, joueurs en tilt : sachez à qui vous avez affaire avant le coup d'envoi, sans jamais toucher au jeu.

Tauri v2 · Rust · SQLite · overlay transparent · source navigateur OBS

---

## Pourquoi c'est compatible Easy Anti-Cheat

LobbyTracker n'interagit **jamais** avec le processus du jeu.

| Interdit (et absent du code) | Ce que fait LobbyTracker |
|---|---|
| Injection de DLL | Lit `Launch.log` en lecture seule (partage de fichier standard) |
| `ReadProcessMemory` / `WriteProcessMemory` | Écoute optionnelle de la **Stats API officielle** (TCP `127.0.0.1`) |
| Hook DirectX / Vulkan | Overlay = fenêtre Windows transparente séparée, 0 coût FPS |
| Requêtes dans la boucle de rendu | Requêtes réseau dans un thread async, hors du jeu, avec cache SQLite |

> Aucun outil ne peut promettre l'absence de risque, et je n'ai pas de garantie d'Epic. Cette conception évite simplement toute technique que les anti-cheats ciblent.

## Fonctionnalités

- **Radar de lobby** : adversaires traités en priorité, puis coéquipiers.
- **Affichage instantané (< 50 ms)** pour les joueurs déjà en cache SQLite (WAL, TTL 24 h) ; les autres se résolvent en arrière-plan avec un voyant discret.
- **Badges, tous désactivables** dans les paramètres. Une catégorie décochée n'est ni calculée ni envoyée à l'overlay :
  | Badge | Règle par défaut |
  |---|---|
  | Smurf suspect | < 250 victoires totales et rang ≥ Champion 2 (seuils réglables) + *Smurf Index* 0-100 |
  | Spécialiste 1v1 | Rang 1v1 ≥ médiane 2v2/3v3 du lobby + 2 tiers (≥ 10 matchs 1v1) |
  | Démolisseur | ≥ 1,6 × la moyenne du lobby et ≥ 0,7 démo/match |
  | Ancre défensive | ≥ 1,6 arrêt/match et arrêts ≥ 2,5 × buts |
  | En tilt | ≥ 4 défaites consécutives |
  | Écart de rang historique | Pic ≥ Champion 2 et rang actuel ≥ 2 tiers sous le pic |
  | Adversaires uniquement | Masque les coéquipiers |
- **Overlay Tauri** : transparent, sans bordure, toujours au-dessus, raccourcis globaux configurables (`F9` afficher/masquer, `Ctrl+Shift+C` click-through), déplacement libre avec position sauvegardée. Quatre dispositions (cartes verticales, cartes horizontales, colonne, ligne), taille de 50 à 200 %, et masquage automatique après N secondes à l'arrivée d'un lobby.
- **OBS** : ajoutez une *Source Navigateur* sur `http://127.0.0.1:49124/obs` (fond transparent, mise à jour par WebSocket).
- **Scraper hybride** : WebView2 invisible (passe Cloudflare comme un vrai navigateur) puis repli sur l'API officielle TRN avec votre jeton.

## Architecture

```
Launch.log ──► tracker-watcher ─┐
Stats API  ──►                  ├─► tracker-parser ─► tracker-engine ─► tracker-heuristics ─► LobbySnapshot
                                │        (IDs)           │  ▲                                   │
                                │                        ▼  │                                   ├─► overlay (événements Tauri)
                                │              tracker-cache (SQLite)                           └─► tracker-server (WS + OBS)
                                │                        ▲
                                └──────── tracker-scraper (file prioritaire, WebView2 / API TRN)
```

| Crate | Rôle |
|---|---|
| `tracker-core` | Types partagés, configuration (`serde_json`), vues |
| `tracker-watcher` | Tail asynchrone de `Launch.log` (détecte troncature/recréation), client Stats API avec reconnexion |
| `tracker-parser` | Extraction des IDs Epic (32 hex), SteamID64, Xbox, PSN ; messages de la Stats API |
| `tracker-cache` | SQLite WAL : joueurs, playlists, horodatage, TTL |
| `tracker-scraper` | Sources WebView2 / API TRN, mapping JSON, file prioritaire |
| `tracker-heuristics` | Smurf Index, badges, filtres, tri |
| `tracker-server` | HTTP + WebSocket sur `127.0.0.1` uniquement |
| `tracker-engine` | Orchestration et état du lobby |
| `src-tauri` + `ui/` | Application, fenêtres, raccourcis, interface |

### Contournement Cloudflare

Tracker Network renvoie 403 aux clients HTTP nus. LobbyTracker ouvre donc l'URL de l'API interne dans une **fenêtre WebView2 masquée** ; une fois le défi passé, un script *de notre propre fenêtre* renvoie le JSON affiché à l'application (commande Tauri `submit_scrape`, restreinte à `api.tracker.gg`, avec repli HTTP vers `127.0.0.1`). Les identifiants de requête sont aléatoires et à usage unique.

> L'utilisation des données de Tracker Network reste soumise à leurs conditions d'utilisation ; vous en êtes responsable. Le jeton API officiel est l'option la plus propre.

## Installation

Prérequis : Windows 10/11 (WebView2 inclus), [Rust](https://rustup.rs) stable, et les [prérequis Tauri v2](https://v2.tauri.app/start/prerequisites/).

```bash
cargo install tauri-cli --version "^2" --locked
cargo tauri dev          # développement
cargo tauri build        # installeur NSIS
```

Pour les équipes en direct (et donc la distinction adversaire / coéquipier), activez la Stats API du jeu : dans `<Rocket League>\TAGame\Config\DefaultStatsAPI.ini`, mettez `PacketSendRate=30` (port par défaut 49123), puis renseignez **votre pseudo** dans les paramètres.

## Tests

```bash
cargo test --workspace
```

Les tests couvrent le parseur (formats d'ID, faux positifs, Stats API), les heuristiques et filtres, le cache (TTL, WAL), la file prioritaire, le tail de fichier, le mapping Tracker Network et le serveur local.

## Limites connues (à lire)

- **Format de `Launch.log`** : il n'est pas documenté. Le parseur s'appuie sur la forme canonique `Plateforme|id|0` et sur les mots-clés `BeaconJoin`, `LogNet`, `LoadMap`. Ces motifs n'ont **pas été validés contre un vrai log** ; ajustez `crates/tracker-parser/src/lib.rs` si votre log diffère. Les noms contenant des espaces sans séparateur peuvent être mal extraits ; la Stats API est la source la plus fiable.
- **Schéma Tracker Network** : lu de façon tolérante, mais non figé. Les démolitions par match ne sont présentes que si le profil les expose ; le nombre de matchs est estimé (2 × victoires) en son absence. Le rang de pic provient de `peak-rating` (estimé depuis le MMR si besoin). La série de défaites vient de l'endpoint `sessions`, dont le format est supposé.
- **Scraper WebView2** : écrit et compilé, mais non testé en conditions réelles contre Cloudflare.
- Les ports et chemins nécessitent un redémarrage ; filtres, jeton, raccourcis et click-through s'appliquent à chaud.

## Licence

MIT
