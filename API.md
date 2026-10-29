# API CyclAI

API JSON sous `/api/v1`, servie par un processus dédié : `cyclai serve-api` (port 3001 par défaut). Le dashboard (`cyclai serve`) ne sert **pas** l'API ; voir [Authentification](#authentification).

- Tout est en lecture seule, sauf la planification de séances (`POST` / `DELETE` sur `/planned`).
- Réponses en JSON ; les erreurs ont la forme `{"error": "message"}` avec le code HTTP adapté (400, 401, 403, 404, 500).
- Dates au format `YYYY-MM-DD` ; `started_at` en ISO 8601 UTC.
- Unités : distances en km, durées en secondes (`duration_s`) ou minutes (`duration_min`), puissances en W, FC en bpm, dénivelé en m.

## Authentification

Chaque requête (y compris sur un chemin inconnu) doit porter `Authorization: Bearer <jwt>` : un JWT **RS256** que le client signe avec sa clé privée RSA (2048 bits minimum). Le serveur ne stocke que les clés publiques.

Claims et en-tête exigés :

| Champ | Valeur |
|---|---|
| en-tête `alg` | `RS256` (le seul accepté) |
| en-tête `kid` | nom de la clé enregistrée côté serveur |
| `iat` | date d'émission (epoch, secondes) |
| `exp` | expiration ; `exp - iat` ≤ **300 s** (tolérance d'horloge 30 s) |

Codes : `401` (jeton absent, invalide, expiré, clé inconnue ou révoquée — message unique `invalid token`), `403` (clé en lecture seule sur un `POST` / `DELETE`).

Gestion des clés (sur le serveur, ex. `docker exec -i cyclai-api cyclai …`) :

```bash
# côté client
openssl genrsa -out cyclai.key 3072
openssl rsa -in cyclai.key -pubout -out cyclai.pub

# côté serveur
cyclai api-key add laptop - --scope read < cyclai.pub    # ou --scope write
cyclai api-key list
cyclai api-key revoke laptop                              # effet immédiat

# test à la main (génère un jeton de 60 s)
TOKEN=$(cyclai api-key token laptop cyclai.key)
curl -H "Authorization: Bearer $TOKEN" http://localhost:3001/api/v1/profile
```

Exemple client Python (`pip install pyjwt cryptography`) :

```python
import time, jwt
now = int(time.time())
token = jwt.encode({"iat": now, "exp": now + 60}, open("cyclai.key").read(),
                   algorithm="RS256", headers={"kid": "laptop"})
```

Les jetons ne sont pas à usage unique : sers l'API en HTTPS (ex. Tailscale Funnel) pour qu'un jeton ne puisse pas être rejoué par un tiers pendant sa courte validité.

| Méthode | Chemin | Rôle |
|---|---|---|
| GET | [`/api/v1/profile`](#get-apiv1profile) | FTP, FC, zones, charge actuelle |
| GET | [`/api/v1/load`](#get-apiv1load) | Série quotidienne TSS / CTL / ATL / TSB |
| GET | [`/api/v1/activities`](#get-apiv1activities) | Dernières sorties |
| GET | [`/api/v1/activities/{id}`](#get-apiv1activitiesid) | Détail d'une sortie : laps détaillés, performances |
| GET | [`/api/v1/activities/{id}/streams`](#get-apiv1activitiesidstreams) | Séries temporelles (puissance, FC, allure…) |
| GET | [`/api/v1/summary`](#get-apiv1summary) | Synthèse des sorties récentes |
| GET | [`/api/v1/workouts`](#get-apiv1workouts) | Modèles de séance |
| GET | [`/api/v1/planned`](#get-apiv1planned) | Séances planifiées |
| POST | [`/api/v1/planned`](#post-apiv1planned) | Planifier une séance |
| DELETE | [`/api/v1/planned/{id}`](#delete-apiv1plannedid) | Retirer une séance du calendrier |

---

## GET /api/v1/profile

Profil physiologique, zones dérivées et charge d'entraînement actuelle.

Pas de paramètre.

```json
{
  "ftp_watts": 212,
  "hr_max": 192,
  "hr_rest": 53,
  "hr_threshold_bpm": 167,
  "hr_tss_coefficient": 1.0,
  "run_threshold_pace": "4:30",
  "run_threshold_pace_s_per_km": 270.0,
  "power_zones": [
    {"zone": 1, "name": "Récupération active", "min_w": 0, "max_w": 117},
    {"zone": 7, "name": "Neuromusculaire", "min_w": 318, "max_w": null}
  ],
  "hr_zones": [
    {"zone": 1, "name": "Récupération", "min_bpm": 53, "max_bpm": 136},
    {"zone": 5, "name": "VO2max/Anaérobie", "min_bpm": 178, "max_bpm": null}
  ],
  "load": {
    "ctl": 41.9,
    "atl": 55.1,
    "tsb": -13.2,
    "tsb_status": "Fatigue modérée, entraînement productif",
    "rest_days_to_next_tsb_band": 1,
    "tss_28d": 1531,
    "power_zone_seconds_28d": [45209, 19319, 14713, 10789, 6326, 5149, 2999],
    "power_zone_pct_28d": [43.3, 18.5, 14.1, 10.3, 6.1, 4.9, 2.9]
  }
}
```

- 7 zones de puissance (Coggan étendu, en % du FTP) ; 5 zones de FC (Karvonen). `max_*` vaut `null` pour la dernière zone.
- `rest_days_to_next_tsb_band` : jours de repos pour passer dans la tranche de TSB supérieure ; `null` dans la tranche la plus haute ou si inatteignable.
- `hr_threshold_bpm` : FC seuil (LTHR) utilisée pour le hrTSS — celle réglée dans `/settings`, ou FC repos + 85 % de la réserve cardiaque si elle n'est pas renseignée.
- `run_threshold_pace` / `run_threshold_pace_s_per_km` : allure seuil en course à pied (réglée dans `/settings`), `null` si non renseignée. Référence des cibles d'allure des séances.
- `hr_tss_coefficient` : coefficient appliqué au hrTSS des courses quand il s'additionne au TSS puissance du vélo (réglable dans `/settings`, 1 = compte pareil, 0 = les courses ne comptent pas).
- La charge (CTL / ATL / TSB, `tss_28d`) cumule le TSS puissance du vélo et le **hrTSS** des courses à pied **pondéré par ce coefficient**.
- `power_zone_*_28d` : temps en zones de puissance, vélo uniquement (une course n'a pas de zones de puissance).

## GET /api/v1/load

Série quotidienne de charge, du plus ancien au plus récent.

| Paramètre | Défaut | Description |
|---|---|---|
| `days` | `90` | Nombre de jours jusqu'à aujourd'hui (1 à 1500) |

```json
{
  "days": 3,
  "series": [
    {"date": "2026-10-04", "tss": 254.8, "ctl": 44.0, "atl": 75.1, "tsb": -31.0},
    {"date": "2026-10-05", "tss": 0.0, "ctl": 43.0, "atl": 64.3, "tsb": -21.4}
  ]
}
```

## GET /api/v1/activities

Dernières sorties, de la plus récente à la plus ancienne.

| Paramètre | Défaut | Description |
|---|---|---|
| `limit` | `10` | Nombre max de résultats (1 à 200) |
| `days` | — | Ne garder que les N derniers jours (sans valeur : tout l'historique) |
| `sport` | — | `ride` ou `run` ; autre valeur ou absent : les deux |

```json
{
  "count": 1,
  "activities": [{
    "id": 20449027159,
    "sport": "ride",
    "sport_type": "Ride",
    "started_at": "2026-10-04T09:52:00Z",
    "title": "Sortie du dimanche",
    "distance_km": 95.3,
    "duration_s": 14760,
    "elevation_gain_m": 1490,
    "avg_pace_min_per_km": null,
    "avg_speed_kmh": 23.2,
    "avg_power_w": 152,
    "normalized_power_w": 167,
    "avg_hr": 156,
    "max_hr": 178,
    "tss": 255,
    "tss_source": "power",
    "tss_load": 255,
    "intensity_factor": 0.79,
    "power_zone_seconds": [4800, 3200, 3300, 2200, 900, 300, 60]
  }]
}
```

- `sport` vaut `ride` ou `run` (`VirtualRide`, le home trainer, compte comme `ride`). `sport_type` garde la valeur fine : `Ride`, `VirtualRide`, `Run`, `TrailRun`. Pour une course : `avg_pace_min_per_km` (ex. `"5:08"`) est renseigné, `avg_speed_kmh` est `null` ; l'inverse pour le vélo.
- `tss_source` : `power` (vélo, basé sur la puissance normalisée et le FTP) ou `hr` (course à pied : **hrTSS**, basé sur la FC — charge TRIMP rapportée à une heure à la FC seuil = 100). Un TSS vaut `null` si la donnée manque (vélo sans capteur de puissance, course sans FC).
- `tss_load` : ce que la sortie ajoute à la charge totale — égal à `tss` pour le vélo, `tss` × `hr_tss_coefficient` pour une course. Le `tss` d'une course reste son hrTSS brut.
- Les champs de puissance et `intensity_factor` sont `null` quand la donnée n'existe pas (toujours le cas pour une course).
- Les sorties importées depuis un fichier FIT/GPX **et** celles synchronisées depuis Garmin Connect ont un `id` négatif. Un `id` positif est un id Strava. L'API n'expose pas l'id Garmin d'origine (`activities.external_id`).

## GET /api/v1/activities/{id}

Une sortie, avec les mêmes champs que ci-dessus plus le détail calculé à partir des flux bruts (puissance, FC, cadence, altitude, distance échantillonnés à la seconde).

```json
{
  "laps": [{
    "index": 2, "start_s": 2259, "duration_s": 96,
    "distance_km": 0.54, "avg_speed_kmh": 20.1, "avg_pace_min_per_km": null,
    "avg_power_w": 184, "max_power_w": 252, "normalized_power_w": 194, "intensity_factor": 0.92,
    "avg_hr": 151, "max_hr": 157,
    "avg_cadence_rpm": 88, "avg_cadence_spm": null,
    "elevation_gain_m": 17, "avg_grade_pct": 3.1,
    "power_zone_seconds": [3, 23, 32, 17, 21, 0, 0],
    "hr_zone_seconds": [0, 33, 63, 0, 0]
  }],
  "performance": {
    "duration_s": 17081,
    "avg_power_w": 132, "max_power_w": 560, "normalized_power_w": 167,
    "variability_index": 1.27, "intensity_factor": 0.79,
    "power_curve_w": {"5s": 524, "15s": 389, "30s": 370, "1min": 330, "5min": 222,
                      "10min": 217, "20min": 196, "60min": 165},
    "avg_hr": 155, "max_hr": 190,
    "hr_zone_seconds": [2069, 4315, 4290, 6168, 239],
    "avg_cadence_rpm": 65, "avg_cadence_spm": null,
    "avg_grade_pct": 0.0,
    "power_hr_decoupling_pct": 4.2
  },
  "segment_efforts": [
    {"segment": "Mont Saint-Quentin", "avg_power_w": 220,
     "delta_vs_pr_pct": -3.2, "delta_vs_recent_avg_pct": 4.1, "delta_vs_last_pct": 1.5}
  ],
  "bilan": "Texte du bilan généré, ou null"
}
```

**Laps** — chaque lap est découpé dans les flux à partir de sa position dans la sortie (`start_s` = secondes écoulées depuis le départ) :
- `avg_power_w` / `max_power_w` / `normalized_power_w` (NP sur 30 s glissantes, `null` si le lap dure moins de 30 s) et `intensity_factor` (NP / FTP) ;
- `avg_hr` / `max_hr`, cadence (`avg_cadence_rpm` pour le vélo, `avg_cadence_spm` en pas/min pour la course), D+, pente moyenne, vitesse ou allure ;
- `power_zone_seconds` (7 zones, vélo) et `hr_zone_seconds` (5 zones Karvonen) : temps passé dans chaque zone.
- Sans flux disponibles, un lap retombe sur le résumé stocké de la sortie (distance, puissance et FC moyennes, qu'ils viennent de Strava ou du FIT) et les autres champs valent `null`.

**Performance** (`null` sans flux) — métriques sur toute la sortie :
- `power_curve_w` : meilleure puissance moyenne sur 5 s à 60 min (`null` si la sortie est plus courte) ;
- `variability_index` : NP / puissance moyenne ;
- `power_hr_decoupling_pct` : baisse du rapport puissance/FC entre la 1ʳᵉ et la 2ᵉ moitié (positif = la FC dérive à puissance égale) ; seulement pour les efforts d'au moins 20 min avec puissance et FC ;
- pour une course : pas de champs de puissance, cadence en `avg_cadence_spm`, allure sur les laps (`avg_pace_min_per_km`).

`segment_efforts` : efforts sur les segments de référence (matching local), vide pour une course. `404` si l'activité n'existe pas.

## GET /api/v1/activities/{id}/streams

Séries temporelles d'une sortie, moyennées par tranches de `step` secondes.

| Paramètre | Défaut | Description |
|---|---|---|
| `step` | `10` | Taille des tranches en secondes (1 à 300) |
| `from_s` | `0` | Début de la fenêtre (secondes depuis le départ) |
| `to_s` | fin de la sortie | Fin de la fenêtre (exclue) |

Pour zoomer sur un lap : `from_s=<start_s>&to_s=<start_s + duration_s>`.

```json
{
  "activity_id": 20449027159, "step_s": 300, "from_s": 0, "count": 57,
  "time_s": [0, 300, 600],
  "distance_km": [0.0, 1.9, 4.1],
  "speed_kmh": [22.2, 29.4, 26.7],
  "pace_min_per_km": null,
  "power_w": [127, 169, 147],
  "hr_bpm": [136, 150, 142],
  "cadence": [80, 85, 83],
  "altitude_m": [210.4, 230.1, 245.7]
}
```

- Une série absente de la sortie vaut `null`. Pour une course : `pace_min_per_km` (allure décimale, `null` à l'arrêt) remplace `speed_kmh`, et il n'y a pas de puissance.
- `404` si l'activité n'existe pas ou n'a pas de flux ; `400` si la fenêtre est vide ou hors de la sortie.

## GET /api/v1/summary

Synthèse de l'entraînement récent.

| Paramètre | Défaut | Description |
|---|---|---|
| `days` | `28` | Fenêtre en jours (1 à 3650) |

```json
{
  "days": 28,
  "since": "2026-09-09",
  "totals": {"activities": 11, "distance_km": 645.2, "duration_s": 96000,
             "elevation_gain_m": 6651, "tss": 1531, "avg_intensity_factor": 0.78},
  "by_sport": {"Ride": {"activities": 10, "...": "..."}, "Run": {"activities": 1, "...": "..."}},
  "by_week": [
    {"week_start": "2026-09-07", "activities": 4, "distance_km": 215.6,
     "duration_s": 32696, "elevation_gain_m": 2251, "tss": 501}
  ],
  "latest_activity": { "...": "même format que /activities" }
}
```

- `by_sport` est indexé par `sport_type` (`Ride`, `VirtualRide`, `Run`, `TrailRun`), que la sortie vienne de Strava, de Garmin ou d'un fichier. Les `tss` (totaux, par sport, par semaine) additionnent le TSS puissance (vélo) et le hrTSS (course) **pondéré par le coefficient hrTSS**, comme la charge.
- `by_week` : semaines débutant le lundi.

## GET /api/v1/workouts

Les modèles de séance réutilisables (créés dans le concepteur ou générés par le digest). Utilise un `id` comme `workout_id` pour planifier.

Pas de paramètre.

```json
{
  "count": 1,
  "workouts": [{
    "id": 4,
    "name": "Over-under 2x12",
    "source": "manual",
    "sport": "cycling",
    "duration_min": 55.0,
    "estimated_tss": 62,
    "estimated_distance_km": 24.5,
    "workout": {"name": "Over-under 2x12", "steps": [ "..." ]}
  }]
}
```

`source` : `manual` (créé dans le concepteur) ou `digest` (généré par le digest hebdomadaire). `sport` : `cycling`, `running` ou `other`. `estimated_tss` est une estimation à partir des puissances cibles moyennes.

## GET /api/v1/planned

Séances planifiées dans l'intervalle `from` ≤ date < `to`, triées par date.

| Paramètre | Défaut | Description |
|---|---|---|
| `from` | aujourd'hui − 7 j | Date de début (incluse) |
| `to` | aujourd'hui + 42 j | Date de fin (exclue) |
| `sport` | — | `cycling`, `running` ou `other` ; absent : tous |

```json
{
  "from": "2026-09-29",
  "to": "2026-11-17",
  "count": 1,
  "planned": [{
    "id": 1,
    "date": "2026-10-09",
    "name": "Sweet spot 2x20",
    "notes": "via api",
    "workout_id": null,
    "source": "api",
    "sport": "cycling",
    "duration_min": 65.0,
    "estimated_tss": 68,
    "estimated_distance_km": 30.7,
    "workout": {"name": "Sweet spot 2x20", "steps": [ "..." ]}
  }]
}
```

- `workout_id` : modèle d'origine, `null` si la séance est unique à son jour ou si le modèle a été supprimé. Une séance planifiée possède sa propre copie des étapes : modifier ou supprimer le modèle ne la change pas.
- `source` : `manual` (interface web) ou `api`.
- `sport` : `cycling`, `running` ou `other`. L'estimation de distance (`estimated_distance_km`) suit le sport : modèle de vitesse pour le cyclisme, allures des étapes pour la course (`null` si l'allure seuil manque pour une étape sans allure), `null` pour « autre ».
- `400` si `from` / `to` ne sont pas des dates `YYYY-MM-DD` ou si `sport` est inconnu.

## POST /api/v1/planned

Planifie une séance sur un jour. Plusieurs séances par jour sont possibles. Réponse : `201` avec la séance créée (même format qu'un élément de `GET /planned`).

Corps JSON (`Content-Type: application/json`) :

| Champ | Obligatoire | Description |
|---|---|---|
| `date` | oui | `YYYY-MM-DD` |
| `workout_id` | un des deux | Copie les étapes du modèle (voir `GET /workouts`) |
| `workout` | un des deux | Séance inline `{"name": "...", "steps": [...]}` (format ci-dessous) |
| `name` | non | Remplace le nom de la séance (pratique avec `workout_id`) |
| `notes` | non | Note libre |
| `sport` | non | `cycling` (défaut), `running` ou `other`. Avec `workout_id`, reprend par défaut le sport du modèle |

Il faut exactement un de `workout_id` ou `workout`.

Exemple :

```bash
curl -X POST http://localhost:3000/api/v1/planned \
  -H 'content-type: application/json' \
  -d '{
    "date": "2026-10-12",
    "notes": "avant la sortie club",
    "workout": {"name": "Sweet spot 2x20", "steps": [
      {"type": "warmup", "duration": {"type": "time", "value": "15min"},
       "target": {"type": "power_range", "low": 120, "high": 150}},
      {"type": "repeat", "count": 2, "steps": [
        {"type": "interval", "duration": {"type": "time", "value": "20min"},
         "target": {"type": "power_range", "low": 185, "high": 195}},
        {"type": "recovery", "duration": {"type": "time", "value": "5min"},
         "target": {"type": "power_range", "low": 100, "high": 120}}]}]}
  }'
```

À partir d'un modèle :

```bash
curl -X POST http://localhost:3000/api/v1/planned \
  -H 'content-type: application/json' \
  -d '{"date": "2026-10-14", "workout_id": 4, "name": "Seuil (version courte)"}'
```

Erreurs : `400` (date ou sport invalide, ni `workout_id` ni `workout` ou les deux, séance invalide) et `404` (`workout_id` inconnu).

### Format d'une séance (`workout`)

```json
{
  "name": "Nom de la séance",
  "steps": [
    {"type": "warmup|interval|recovery|cooldown",
     "duration": {"type": "time", "value": "15min"},
     "target": {"type": "power_range", "low": 120, "high": 150},
     "terrain": "plat|vallonné|montée",
     "note": "optionnel"},
    {"type": "repeat", "count": 3, "steps": [ "... étapes, y compris d'autres repeat ..." ]}
  ]
}
```

- `duration.value` : `15min`, `90s`, `1h`, `1h30`, `1:30` (m:ss) ou un nombre de minutes.
- `target` (facultatif), au choix :
  - `{"type": "power_range", "low": 185, "high": 195}` — puissance en watts ;
  - `{"type": "power_zone", "zone": 4}` — zone de puissance 1 à 7 (Coggan étendu, en % du FTP : 1 < 55 %, 2 < 75 %, 3 < 90 %, 4 < 105 %, 5 < 120 %, 6 < 150 %, 7 au-delà) ;
  - `{"type": "hr_range", "low": 150, "high": 160}` — fréquence cardiaque en bpm ;
  - `{"type": "hr_zone", "zone": 2}` — zone de FC 1 à 5 (Karvonen, d'après FC repos/max : 1 < 60 % de la réserve, 2 < 70 %, 3 < 80 %, 4 < 90 %, 5 au-delà) ;
  - `{"type": "pace_range", "low": "4:25", "high": "4:35"}` — allure de course en secondes par km (nombre) ou texte `m:ss` ; `low` est le côté **rapide** (le plus petit nombre de secondes), `low` ≤ `high`. Réponses : toujours en secondes (`265.0`).

  `low` ≤ `high` pour les plages. Les cibles FC et allure n'ont pas de watts : leur hauteur sur le graphique et les estimations de TSS utilisent une intensité équivalente — puissance typique de la zone FC (≈ 50 / 65 / 82 / 97 / 110 % du FTP pour les zones 1 à 5), ou allure seuil / allure de l'étape pour une cible d'allure (0,75 si l'allure seuil n'est pas réglée).
- `terrain` (défaut `plat`) et `note` sont facultatifs.
- Il faut un nom non vide, au moins une étape, des durées valides et `count` ≥ 1 avec au moins une étape dans chaque `repeat`.

## DELETE /api/v1/planned/{id}

Retire une séance du calendrier. `204` sans corps en cas de succès, `404` si elle n'existe pas.
