# Architecture

Vue d'ensemble technique de CyclAI : flux de données, modules, schéma SQLite, conventions et décisions volontaires (à ne pas « corriger » par erreur). Les fonctionnalités vues de l'utilisateur sont dans [FONCTIONNALITES.md](FONCTIONNALITES.md).

## Flux

```
Strava ──sync──┐                         ┌── bilan d'activité (à la demande) ──┐
FIT / GPX ─────┴─> SQLite ─> metrics ─>  │                                     ├─> Anthropic (ou rapport IA .md)
                     │        segments   └── digest hebdo (bouton) ────────────┘
                     │                                   │
                     └──────────── web (axum) <──────────┘   +  /api/v1 (JSON)
```

1. **Ingestion** (`sync`, `web::import`) : activité + streams + laps écrits en base.
2. **Calculs déterministes** (`metrics`, `segments`) : TSS/IF/hrTSS, zones, charge quotidienne, résumé du dashboard, efforts sur segments. Toujours recalculés sur l'**historique complet**.
3. **LLM** (`llm`, `digest`) : uniquement à la demande de l'utilisateur ; reçoit des agrégats déjà calculés, jamais l'historique brut.
4. **Présentation** (`web`) : HTML généré côté serveur avec `format!`, sans moteur de gabarits ni étape de build front. Toutes les pages passent par `web::layout::Page` (en-tête, navigation, `BASE_CSS`) ; une page n'ajoute que son CSS / ses scripts propres.

Le sync n'est **pas** un planificateur embarqué : c'est une sous-commande CLI (`cyclai sync`) déclenchée par un timer externe, ou le bouton du dashboard.

## Modules

```
src/
  main.rs            CLI (clap) : serve | sync [--full] | import-segments ; amorçage de la base
  config.rs          variables d'environnement d'amorçage (lues une fois)
  settings.rs        réglages en base : clés, profil, prompts (table settings)
  db/                connexion sqlx, migrations, seeds, rattrapage des miniatures

  strava/            client API : OAuth (refresh rotatif), activités, streams, laps, segments favoris
  sync.rs            orchestration du sync (normal et --full)
  importer.rs        parseurs FIT (fitparser) et GPX (gpx) -> même forme que le sync Strava

  metrics/           TSS/IF, hrTSS, zones puissance, CTL/ATL/TSB, résumé dashboard
  segments/          import des segments favoris + matching haversine local
  bests.rs           records par activité (puissance par durée, meilleur temps par distance)
  performance.rs     flux rééchantillonnés à 1 Hz : stats par lap, courbe de puissance, VI, découplage
  route_thumb.rs     miniature SVG du tracé (liste d'activités)
  route_profile.rs   profil d'altitude / pente d'un GPX d'itinéraire

  workout.rs         modèle Workout (étapes, cibles), durées, TSS / distance estimés
  planned.rs         séances planifiées : validation et stockage

  llm/               trait LlmClient, client Anthropic, prompt du bilan
  digest.rs          prompt et parsing du digest, distance estimée
  report.rs          « rapport IA » markdown (mode sans clé)

  web/
    layout.rs        gabarit commun (barre de navigation, conteneur) et feuille de style de base
    mod.rs           routes, dashboard, glossaire d'infobulles, barres de zones, jauge TSB
    detail.rs        page d'une activité
    workout.rs       liste et concepteur de séances
    workout_chart.rs graphique en échelons partagé (JS)
    planned.rs       formulaire de séance planifiée
    route.rs         page /route
    settings.rs      page /settings
    stats.rs         page /stats (totaux, carte des traces, records, radar)
    import.rs        upload FIT / GPX
    api.rs           API JSON /api/v1
    format.rs        dates en français (Europe/Paris), durées, allures
```

### Qui écrit quoi

| Fichier | Rôle clé |
|---|---|
| `metrics::run` | Point d'entrée des calculs : hrTSS des courses, TSS/IF des sorties, zones, `daily_load`, `dashboard_summary` |
| `segments::run` | Reconstruit `segment_efforts` de zéro, activités triées par date |
| `sync::run_inner` | Auth Strava → liste → filtre sports → insertion → `metrics::run` → `segments::run` |
| `llm::generate_activity_bilans` | Un appel LLM par activité demandée → `activity_bilans` |
| `digest::generate` | Appel LLM → `digests` (+ `workouts` si le schéma est valide) |
| `planned::prepare` | Valide une séance (dates, étapes, cibles, sport) avant insertion |

## Modèle de données (SQLite)

Les migrations de `migrations/` sont embarquées dans le binaire et appliquées au démarrage. Tables à une seule ligne (`CHECK (id = 1)`) : `athlete_config`, `settings`, `sync_state`, `dashboard_summary`.

| Table | Contenu |
|---|---|
| `activities` | Une ligne par sortie : horodatage, `sport_type`, durée, distance, D+, puissance moy / NP, FC, `tss` + `tss_source` (`power` \| `hr`), `intensity_factor`, `zone1_s`…`zone7_s`, `title`, `description`, `user_context`, `route_thumb_svg`, `route_points_json` (trace simplifiée), `bests_computed`. Les imports FIT / GPX ont un **id négatif** |
| `activity_streams` | `streams_json` brut (même forme que l'API Strava : `{clé: {data: [...]}}`) ; volumineux, jamais joint pour la liste |
| `laps` | Laps (durée, distance, puissance et FC moyennes) |
| `reference_segments` | Segments favoris : nom, coordonnées départ / arrivée, distance, D+ |
| `segment_efforts` | Effort détecté : bornes temps / distance, puissance, vitesse, FC, deltas vs record / moyenne / dernier, activités de référence |
| `activity_bilans` | Texte du bilan LLM par activité |
| `digests` | Note libre, résumé envoyé, réponse JSON et texte |
| `workouts` | Modèles de séances (`steps_json`, `sport`, `source` = `manual` \| `digest`) |
| `planned_workouts` | Séances planifiées : copie des étapes, date, `sport`, `workout_id` informatif, `source` = `manual` \| `api` |
| `activity_bests` | Records par activité : `power` (durée en s → W) et `dist` (distance en m → meilleur temps en s) |
| `daily_load` | TSS, CTL, ATL, TSB par jour, de la première activité à aujourd'hui |
| `dashboard_summary` | Cache d'une ligne pour la carte « 4 dernières semaines » |
| `athlete_config` | FTP, FC max / repos / seuil, coefficient hrTSS, allure seuil de course |
| `settings` | Clés Strava / Anthropic, profil cycliste, deux prompts système |
| `sync_state` | Date du dernier sync, refresh token Strava (rotatif) |

`steps_json` est volontairement du JSON non normalisé : un arbre d'étapes avec répétitions imbriquées s'adapte mal à des tables.

## Décisions volontaires

À respecter avant de « simplifier » :

- **FTP manuel**, stocké en base (pas en dur, pas l'auto-FTP Strava / Garmin) — réglable après un retest, et tout l'historique est recalculé.
- **Matching de segments local**, jamais les `segment_efforts` de Strava.
- **Pas d'authentification applicative** : accès VPN uniquement.
- **Zones de puissance à 7 niveaux** (Coggan étendu) ; zones de FC à 5 niveaux (Karvonen) — deux modèles distincts.
- **Distance estimée calculée en Rust**, jamais par le LLM.
- **Le LLM ne reçoit que des agrégats** déjà calculés, avec le profil cycliste ; le prompt interdit de supposer l'intention d'une sortie.
- **Sync par CLI externe**, pas de scheduler embarqué.
- **Bilans à la demande**, pas au sync : permet d'ajouter du contexte et évite un backfill payant (le premier sync complet avait généré 221 appels d'un coup).
- **Format de séance stable** (cf. [FONCTIONNALITES.md](FONCTIONNALITES.md#format-dune-séance)) : partagé par le digest, le concepteur, le calendrier et l'API.
- **Pas d'intégration Garmin** : retirée.
- **Durées affichées en `H:MM:SS`**.
- **Réutiliser des bibliothèques** plutôt que réinventer (Chart.js, Leaflet, FullCalendar) ; seule la miniature de tracé est un SVG maison, pour éviter un fond de carte sur 10 vignettes.

### Couleurs et thème

L'interface n'utilise que des variables CSS (`--bg`, `--card-bg`, `--text`, `--accent`…, définies dans `web/layout.rs`). La couleur d'accent et le mode (`auto` / `light` / `dark`) sont stockés dans `settings.theme_color` / `theme_mode`, gardés en mémoire (`layout::set_theme`) et injectés à chaque page : `<html data-theme>` + `--accent` ; les nuances dérivées (`--accent-hover`, `--accent-soft`) viennent de `color-mix()` et la couleur du texte sur l'accent est choisie par luminance. Le JavaScript lit les variables via `cssVar('--x')` (canvas et Leaflet ne comprennent pas `var()`), Chart.js reçoit ses couleurs par défaut au chargement, et les tuiles de carte sont inversées en mode sombre.

Couleurs de données, fixes quel que soit le thème :

| Donnée | Couleur |
|---|---|
| FC | rouge `#dc2626` |
| Vitesse | bleu clair `#38bdf8` |
| Cadence | orange `#f97316` |
| Puissance | mauve `#a855f7` |
| Altitude | gris `#9ca3af` (aire pleine) |
| Zones de puissance Z1→Z7 | gris, bleu, vert, jaune, orange, rouge, mauve |
| Zones de FC Z1→Z5 | gris, bleu, vert, orange, rouge |
| Course à pied | teal `#0d9488` |

## Dépendances

**Crates** : `axum` (multipart), `tokio`, `sqlx` (sqlite, migrate), `reqwest`, `serde` / `serde_json`, `chrono` + `chrono-tz`, `clap`, `anyhow`, `thiserror`, `tracing`, `dotenvy`, `regex` (glossaire), `fitparser`, `gpx`.

**Chargées par le navigateur depuis un CDN** : Chart.js 4.4.4 et chartjs-plugin-annotation 3.0.1 (jsDelivr), FullCalendar 6.1.15 (jsDelivr), Leaflet 1.9.4 (unpkg), tuiles OpenStreetMap. Le serveur n'a pas besoin d'Internet pour les servir, mais **le navigateur, si** : hors ligne, graphes, carte et calendrier ne s'affichent pas.

## Déploiement

- Image multi-étape (`Dockerfile`) : build Rust, runtime `debian:bookworm-slim`, volume `/data` pour SQLite, ports 3000 (UI) et 3001 (API), entrée `cyclai serve`.
- Deux modes, deux processus (`docker-compose.yml`, même image, même volume `/data`) : `serve` = UI sans API, `serve-api` = API seule derrière `web::auth::require_api_key`. Adresse d'écoute : `--bind` / `BIND_ADDR`. SQLite (WAL) est partagé entre les deux ; le volume doit être local.
- Auth de l'API : JWT RS256, clés publiques dans la table `api_keys` (`apikeys.rs`), lues à chaque requête (révocation immédiate), durée de vie max 300 s, portée `read` / `write`.
- `update.ps1` construit l'image, l'exporte en `cyclai.tar`, la charge dans Portainer via son API, puis **redéploie la stack** `cyclai` (un simple stop/start réutiliserait l'ancienne image ; le redéploiement recrée les conteneurs dont l'image a changé).
  > Le script contient une clé d'API Portainer en clair : il est dans `.gitignore`, ne le commite pas.

## Tests

`cargo test` : 75 tests, sur une base SQLite en mémoire (migrations réelles) — calculs de charge, hrTSS, zones, matching de segments, parseurs FIT (`tests/fixtures/`) et GPX, durées de séances, cibles, planification, API, jauge TSB, rapports IA. Les appels LLM passent par un faux client.

## Pièges connus

- Raw strings Rust `r#"…"#` contenant `"#` (ex. `stroke="#2563eb"`) : utiliser `r##"…"##`.
- Anthropic : la réflexion étendue est activée par défaut sur certains comptes et produit un bloc `thinking` sans `text` — la requête la désactive et le parsing ne garde que les blocs `text`.
- Une réponse de digest tronquée par `max_tokens` casse le JSON en silence ; la limite est à 4096.
- `daily_load` doit être vidée avant reconstruction, sinon des lignes antérieures à la première activité subsistent.
- Un lap de moins d'une minute doit passer par `format_zone_duration`, pas par une division entière.
