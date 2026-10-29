# CyclAI

Application **self-hosted, mono-utilisateur**, qui synchronise les activités depuis Strava ou Garmin Connect (vélo de route, home trainer et course à pied), calcule des métriques d'entraînement déterministes (TSS/IF, zones, CTL/ATL/TSB) et génère bilans et séances avec Anthropic (Claude) ou OpenAI — ou sans clé, via un « rapport IA » à coller dans n'importe quel assistant.

Écrite en Rust (axum + SQLite/sqlx), une seule image Docker, pas de JavaScript à builder. Prévue pour tourner sur un Raspberry Pi / NAS derrière un VPN : **il n'y a volontairement aucune authentification applicative**.

## Fonctionnalités

| | |
|---|---|
| **Sources** | Sync Strava (OAuth) **ou** Garmin Connect (clé personnelle) — une seule à la fois — plus upload manuel de fichiers FIT / GPX |
| **Métriques** | TSS / IF avec un FTP *manuel*, zones de puissance (7) et de FC (5), CTL / ATL / TSB (Banister), hrTSS pour la course |
| **Segments** | Matching GPS 100 % local (haversine), avec deltas vs record / moyenne / dernier passage. Le catalogue de segments favoris s'importe encore depuis Strava |
| **Bilan d'activité** | Texte qualitatif généré à la demande depuis la page d'une sortie, avec contexte libre optionnel |
| **Digest hebdo** | Séance structurée (JSON) proposée à partir de la charge des 4 dernières semaines |
| **Concepteur de séances** | Éditeur de séances par échelons (puissance, zones, FC, allure), profil graphique, TSS / durée / distance estimés |
| **Calendrier** | Sorties réalisées + séances planifiées (cliquer sur un jour pour planifier) |
| **Statistiques** | Distance totale vélo / course, volume par mois, toutes les traces sur une carte, records de vitesse sur distance, étoile de puissance par durée |
| **Itinéraire** | Charge un GPX : carte, profil d'altitude coloré par pente, D+ / D− |
| **API JSON** | `/api/v1/*` pour brancher un autre outil ou un agent — voir [API.md](API.md) |
| **Sans clé API** | Boutons « Télécharger le rapport IA » : la consigne + les données exactes, en markdown |

Détail de chaque fonctionnalité : [docs/FONCTIONNALITES.md](docs/FONCTIONNALITES.md).

## Démarrage rapide

Prérequis : Rust stable (édition 2024) ou Docker.

```bash
cp .env.example .env   # ou créer .env à la main, voir « Configuration »
cargo run -- serve     # dashboard sur http://localhost:3000
```

Au premier lancement la base `cyclai.db` est créée et les migrations appliquées. Tout se règle ensuite depuis **Paramètres** (`/settings`) : source de synchro, clés Strava / Garmin / LLM, FTP, FC, profil cycliste, prompts.

### Configuration initiale (`.env`)

Ces variables ne servent qu'à **amorcer** la base au tout premier démarrage ; ensuite `/settings` fait foi et l'environnement n'est plus relu.

| Variable | Défaut | Rôle |
|---|---|---|
| `DATABASE_URL` | `sqlite://cyclai.db` | Fichier SQLite |
| `STRAVA_CLIENT_ID`, `STRAVA_CLIENT_SECRET` | — | App créée sur developers.strava.com |
| `STRAVA_REFRESH_TOKEN` | — | Voir « Connecter Strava » |
| `ANTHROPIC_API_KEY` | — | Optionnelle. Fournisseur par défaut s'il n'y a pas de clé OpenAI |
| `OPENAI_API_KEY` | — | Optionnelle. Devient le fournisseur actif seulement si aucune clé Anthropic n'est amorcée |
| `GARMIN_API_KEY` | — | Clé personnelle Garmin Connect. Devient la source active seulement si Strava n'est pas amorcé |
| `FTP_WATTS` | `200` | FTP **manuel** (jamais l'auto-FTP Strava / Garmin) |
| `HR_MAX`, `HR_REST` | `190`, `50` | Pour les zones Karvonen et le hrTSS |
| `ATHLETE_PROFILE` | — | Profil libre réinjecté dans chaque prompt LLM |

Aucune variable n'est obligatoire : sans rien, l'app démarre et se configure depuis le navigateur.

### Connecter Strava

1. Crée une app sur <https://www.strava.com/settings/api> → `client_id` / `client_secret`.
2. Autorise-la avec le scope **`read,activity:read_all`** (le scope `read` seul donne un 401 sur `/athlete/activities`) et échange le code contre un `refresh_token`.
3. Saisis les trois valeurs dans `/settings`. Strava fait tourner le refresh token à chaque usage : CyclAI stocke le nouveau dans la table `sync_state`.
4. Dashboard → **Synchroniser Strava**. Pour récupérer aussi l'historique ancien (ex. courses d'avant le support de la course à pied) : **Tout l'historique**.
5. Étoile tes segments de référence sur Strava, puis `cyclai import-segments`.

### Connecter Garmin Connect

1. Sur Garmin Connect : **Paramètres → Personal API**, crée une clé personnelle (elle expire au bout de 30, 90, 180 ou 365 jours selon le choix).
2. Colle-la dans `/settings` (ou dans `GARMIN_API_KEY` avant le tout premier démarrage).
3. Choisis **Garmin Connect** comme source de synchronisation. Une seule source est interrogée : les sorties déjà en base restent.
4. Dashboard → **Synchroniser Garmin**. Après un changement de source, **Tout l'historique** une fois : la synchro simple ne reprend que ce qui est postérieur à la dernière synchro, quelle que soit la source.
5. Garmin Connect n'expose pas de catalogue de segments étoilés. `import-segments` reste une commande Strava ; le matching local continue sur les segments déjà importés.

Les sorties Garmin reçoivent un id interne négatif (les id Garmin sont trop grands pour cohabiter avec les id Strava). Le lien vers Connect utilise l'id Garmin d'origine.

## Commandes

| Commande | Rôle |
|---|---|
| `cyclai serve` | Dashboard web seul (sans API) sur `0.0.0.0:3000` (`--bind` / `BIND_ADDR`) |
| `cyclai serve-api` | API JSON seule sur `0.0.0.0:3001`, authentifiée par JWT RS256 — voir [API.md](API.md#authentification) |
| `cyclai api-key add\|list\|revoke\|token` | Gère les clés publiques autorisées à appeler l'API |
| `cyclai sync` | Récupère les nouvelles activités de la source choisie dans `/settings` (Strava ou Garmin), recalcule les métriques, relance le matching de segments |
| `cyclai sync --full` | Idem sur tout l'historique de cette source (seules les activités absentes sont importées ; reprend après une limite de débit) |
| `cyclai import-segments` | Importe les coordonnées de tes segments **favoris Strava** dans `reference_segments` |

Le sync ne génère **pas** de bilan LLM : un bilan se déclenche depuis la page de l'activité (voir [docs/FONCTIONNALITES.md](docs/FONCTIONNALITES.md#bilan-dactivité)), pour pouvoir ajouter du contexte avant de payer l'analyse.

## Déploiement

### Docker

```bash
docker build -t cyclai .
docker run -d --name cyclai -p 3000:3000 -v cyclai-data:/data cyclai
```

La base vit dans le volume `/data` (`DATABASE_URL=sqlite:///data/cyclai.db`). Les variables de l'amorçage ci-dessus peuvent être passées avec `-e` ou `--env-file`.

Sync quotidien depuis l'hôte (timer systemd, cron…) :

```bash
docker exec cyclai cyclai sync
```

Exemple de timer systemd (`/etc/systemd/system/cyclai-sync.{service,timer}`) :

```ini
# cyclai-sync.service
[Service]
Type=oneshot
ExecStart=/usr/bin/docker exec cyclai cyclai sync

# cyclai-sync.timer
[Timer]
OnCalendar=*-*-* 06:30:00
Persistent=true
[Install]
WantedBy=timers.target
```

`update.ps1` (local, ignoré par git) automatise le déploiement : build, export de l'image, envoi à Portainer puis redéploiement de la stack `cyclai` ; voir [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md#déploiement).

### Sécurité

Le dashboard (`serve`) n'a ni authentification ni TLS : n'expose jamais le port 3000 sur Internet, uniquement sur un réseau de confiance / VPN. Les clés (Strava, Garmin, Anthropic, OpenAI) sont stockées **en clair** dans SQLite et affichées dans `/settings`.

Seule l'API (`serve-api`) est faite pour être exposée : elle ne sert rien d'autre que `/api/v1` et exige un JWT RS256 par requête. `docker-compose.yml` lance les deux processus sur la même base : l'UI dans le tailnet (`tailscale serve`), l'API éventuellement en funnel (`tailscale funnel`).

## Pages

| Route | Contenu |
|---|---|
| `/` | Charge 4 semaines (TSS, CTL/ATL/TSB, jauge de forme, zones), calendrier, activités récentes |
| `/activity/{id}` | Graphe superposé interactif, carte, laps, zones, courbe de puissance, segments, bilan |
| `/workouts`, `/workouts/{id}` | Digest hebdomadaire, modèles de séances et concepteur |
| `/planned/new`, `/planned/{id}` | Planification d'une séance sur un jour |
| `/stats` | Statistiques : totaux, traces sur la carte, records, radar de puissance |
| `/route` | Analyse d'un GPX d'itinéraire (rien n'est enregistré) |
| `/settings` | Clés, FTP / FC / allure seuil, profil, prompts système |
| `/api/v1/*` | API JSON ([API.md](API.md)) |

## Développement

```bash
cargo test            # calculs, parseurs FIT/GPX, API, migrations sur SQLite en mémoire
cargo run -- serve    # pas de rechargement à chaud : relancer après modification
```

- Les requêtes utilisent `sqlx::query` / `query_as` à l'exécution (pas la macro `query!`) : aucune base n'est nécessaire à la compilation.
- Les migrations de `migrations/` sont embarquées au build (`sqlx::migrate!`). `db::adopt_squashed_history` ne reprend qu'une base antérieure au squash ; Garmin et OpenAI sont des migrations suivantes. Le `Dockerfile` force la recompilation quand elles changent.
- Les tests FIT s'appuient sur `tests/fixtures/*.fit`.
- Les LLM sont derrière le trait `LlmClient` : les tests utilisent un faux client, jamais l'API payante.

## Documentation

- [docs/FONCTIONNALITES.md](docs/FONCTIONNALITES.md) — comportement détaillé de chaque fonctionnalité
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — modules, flux de données, schéma SQLite, décisions volontaires
- [API.md](API.md) — référence de l'API JSON
- [summary.md](summary.md) — état d'avancement et reste à faire

## Hors périmètre (volontaire)

- Envoi de séances vers Garmin Connect : les séances se saisissent dans CyclAI (concepteur, calendrier).
- Authentification du dashboard (accès VPN uniquement) ; seule l'API exposée est authentifiée.
- `segment_efforts` calculés par Strava : tout le matching est local.
- Module salle de sport : retiré.
