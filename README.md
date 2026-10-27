# LobbyTracker

**Français** · [English](README.en.md)

**Le radar d'avant-match passif pour Rocket League.** Rang réel, smurfs, menaces 1v1, joueurs en lose streak : sachez à qui vous avez affaire avant le coup d'envoi, sans jamais toucher au jeu.

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
  | Potentiel smurf | < 250 victoires totales et rang ≥ Champion 2 (seuils réglables), ou compte récent en montée rapide (≥ 15 matchs classés récents, ≥ 70 % de victoires, ≥ +150 MMR) + *Smurf Index* 0-100 |
  | Joueur 1v1 | Rang 1v1 ≥ médiane 2v2/3v3 du lobby + 2 tiers (≥ 10 matchs 1v1) |
  | Gardien | ≥ 1,6 arrêt/match et arrêts ≥ 2,5 × buts |
  | Lose streak | ≥ 3 défaites consécutives (affiche le nombre) |
  | Ancien haut rang | Pic ≥ Champion 2 et rang actuel ≥ 2 tiers sous le pic |
  | Adversaires uniquement | Masque les coéquipiers |
- **Overlay Tauri** : transparent, sans bordure, toujours au-dessus, raccourcis globaux configurables (`F9` afficher/masquer, `Ctrl+Shift+C` click-through), déplacement libre avec position sauvegardée. Quatre dispositions (cartes verticales, cartes horizontales, colonne, ligne), taille de 50 à 200 %, opacité et couleurs personnalisables, et masquage automatique à l'arrivée d'un lobby : au choix dès la fin du décompte 3-2-1 (Stats API activée, événement `RoundStarted`) ou après N secondes.
- **OBS** : ajoutez une *Source Navigateur* sur `http://127.0.0.1:49125/obs` (fond transparent, mise à jour par WebSocket).
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
| `tracker-gamecfg` | Détection des installations Epic / Steam, réglage de `PacketSendRate` |
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

Pour les équipes en direct (et donc la distinction adversaire / coéquipier), la Stats API du jeu doit être activée. Dans les paramètres, la section **Stats API de Rocket League** détecte les installations Epic Games et Steam et règle `PacketSendRate` dans `DefaultStatsAPI.ini` (10 / 30 / 60 par seconde) ; une copie `.lobbytracker.bak` est conservée. Relancez le jeu après le premier réglage ; ensuite, LobbyTracker remet la valeur en place si une mise à jour du jeu l'efface. Votre compte est détecté automatiquement.

### Mises à jour automatiques

L'application cherche une nouvelle version au démarrage (GitHub Releases) et propose de l'installer ; la signature est vérifiée avant installation. Pour publier des mises à jour, ajoutez dans les secrets du dépôt `TAURI_SIGNING_PRIVATE_KEY` (et `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`, vide si la clé n'a pas de mot de passe), puis publiez la release (non brouillon) créée par le workflow. Les builds locales signent avec la même variable d'environnement.

## Tests

```bash
cargo test --workspace
```

Les tests couvrent le parseur (formats d'ID, faux positifs, Stats API), les heuristiques et filtres, le cache (TTL, WAL), la file prioritaire, le tail de fichier, le mapping Tracker Network et le serveur local.

## Limites connues (à lire)

- **Format de `Launch.log`** : il n'est pas documenté. Le parseur s'appuie sur la forme canonique `Plateforme|id|0` et sur les mots-clés `BeaconJoin`, `LogNet`, `LoadMap`. Ces motifs n'ont **pas été validés contre un vrai log** ; ajustez `crates/tracker-parser/src/lib.rs` si votre log diffère. Les noms contenant des espaces sans séparateur peuvent être mal extraits ; la Stats API est la source la plus fiable.
- **Schéma Tracker Network** : vérifié sur un vrai profil (rang par mode, rang de pic, moyennes par mode, historique `sessions`), mais rien ne garantit qu'il reste stable. Les comptes Epic, Xbox et PSN sont cherchés par pseudo : un joueur dont le pseudo n'est pas encore connu reste en « chargement ».
- **Scraper WebView2** : écrit et compilé, mais non testé en conditions réelles contre Cloudflare.
- Les ports et chemins nécessitent un redémarrage ; filtres, jeton, raccourcis et click-through s'appliquent à chaud.

## Licence

MIT
