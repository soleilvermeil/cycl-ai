# Fonctionnalités

Comportement détaillé de CyclAI. Pour la vue d'ensemble et l'installation, voir le [README](../README.md) ; pour le fonctionnement interne, [ARCHITECTURE.md](ARCHITECTURE.md).

- [Sources d'activités](#sources-dactivités)
- [Métriques d'entraînement](#métriques-dentraînement)
- [Segments de référence](#segments-de-référence)
- [Bilan d'activité](#bilan-dactivité)
- [Digest hebdomadaire](#digest-hebdomadaire)
- [Mode sans clé API : rapport IA](#mode-sans-clé-api--rapport-ia)
- [Concepteur de séances](#concepteur-de-séances)
- [Séances planifiées et calendrier](#séances-planifiées-et-calendrier)
- [Page d'une activité](#page-dune-activité)
- [Statistiques](#statistiques)
- [Itinéraire GPX](#itinéraire-gpx)
- [Paramètres](#paramètres)

## Sources d'activités

Une seule source est active, choisie dans `/settings` (**Source de synchronisation**). `cyclai sync` et le bouton du dashboard interrogent celle-là. Les sorties déjà en base restent. La date de dernière synchro est partagée : après un changement de source, **Tout l'historique** une fois, sinon seules les sorties postérieures à cette date sont demandées.

| Source | Comment | Notes |
|---|---|---|
| **Strava** | Bouton « Synchroniser Strava » ou `cyclai sync` | Ne garde que `sport_type` = `Ride`, `VirtualRide`, `Run`, `TrailRun` (le champ historique `type` confond route et gravel, il n'est pas utilisé). Id Strava = clé primaire (positif), lien vers strava.com |
| **Garmin Connect** | Bouton « Synchroniser Garmin » ou `cyclai sync` | Clé personnelle (Paramètres Connect → Personal API). Même filtre de sports, via le type Garmin (voir ci-dessous). Id interne **négatif** ; le lien Connect utilise l'id Garmin (`external_id`) |
| **Import FIT / GPX** | Bouton « Importer GPX/FIT » du dashboard | FIT : puissance, FC, cadence, laps. GPX : position, altitude, temps seulement. Id **négatif**, `source = file`, pas de lien externe |

Pour chaque activité Strava nouvelle, le sync récupère le résumé, la description, les streams (`latlng, distance, watts, heartrate, altitude, cadence, time`) et les laps (≈ 3 appels). Une activité déjà stockée est ignorée. Après l'import, les métriques et les segments sont recalculés sur **tout** l'historique.

`sync --full` ignore la date du dernier sync et parcourt tout l'historique de la source active. Limite de débit Strava (100 requêtes / 15 min) : relancer reprend avec ce qui manque. Garmin répond parfois 429 : CyclAI réessaie (en-tête `Retry-After`, sinon un délai qui double).

### Garmin Connect

La clé personnelle sert à obtenir un jeton de 24 h à chaque synchro (pas de refresh token). Elle expire selon la durée choisie à la création (30, 90, 180 ou 365 jours). Usage personnel, un seul compte.

CyclAI ne lit pas les flux JSON de Connect : il télécharge l'archive ZIP de l'activité, en extrait le FIT, et le passe au même parseur que l'import fichier. Une archive absente ou illisible est ignorée et retentée à la synchro suivante.

Correspondance des types Garmin conservés :

| Type Garmin | `sport_type` CyclAI |
|---|---|
| `cycling`, `road_biking` | `Ride` |
| `indoor_cycling`, `virtual_ride` | `VirtualRide` |
| `running`, `treadmill_running`, `track_running` | `Run` |
| `trail_running`, `ultra_run` | `TrailRun` |

Gravel, VTT, vélo électrique et cardio indoor sont ignorés. Sur une course, la puissance de la montre est retirée des flux et des laps : le hrTSS reste basé sur la FC. Un vélo sans capteur de puissance n'a pas de TSS, comme avec Strava.

Les id Garmin sont de grands entiers positifs. Ils ne peuvent pas servir de clé primaire (collision avec Strava, et un id positif est un lien strava.com). L'id interne est donc négatif, partagé avec les imports fichier ; `activities.external_id` garde l'id Connect. Index unique `(source, external_id)`.

### Vélo et course à pied

- **Vélo (`Ride`, et `VirtualRide` pour le home trainer)** : pipeline complet — puissance, TSS/IF, zones, segments, bilan. Sans capteur de puissance, pas de TSS.
- **Course (`Run`, `TrailRun`)** : allure en **min/km** (liste, détail, laps, courbe d'allure) et FC. Pas de puissance, de zones de puissance ni de segments. Le bilan ne parle que d'allure, de FC et de dénivelé.

## Métriques d'entraînement

Tout est calculé localement et de façon déterministe ; les calculs sont refaits sur l'historique complet à chaque sync, import ou changement de réglage athlète (FTP, FC…), donc **modifier le FTP réécrit rétroactivement TSS, IF et zones**.

- **TSS / IF** (vélo) : à partir de la puissance normalisée et du **FTP manuel** de `/settings`. L'auto-FTP de Strava / Garmin n'est jamais utilisé.
- **Zones de puissance** : 7 zones de Coggan en % du FTP — <55 / 55-75 / 75-90 / 90-105 / 105-120 / 120-150 / >150 %.
- **Zones de FC** : 5 zones Karvonen (réserve cardiaque = FC max − FC repos), calculées à la volée sur la page détail.
- **hrTSS** (course) : charge TRIMP rapportée à une heure à la FC seuil = 100, stockée dans `activities.tss` avec `tss_source = 'hr'` (le TSS puissance du vélo est `'power'`). La **FC seuil** (LTHR) se règle dans `/settings` ; `0` = automatique (FC repos + 85 % de la réserve). Une course sans FC n'a pas de TSS et ne casse rien.
- **Coefficient hrTSS** (`/settings`, 1 par défaut) : pondère le hrTSS quand il est **additionné** au TSS vélo dans la charge totale (CTL/ATL/TSB, TSS 28 j, totaux API). 0,8 = les courses comptent 20 % de moins, 0 = pas du tout. Le hrTSS affiché sur la course reste le score brut (l'API expose la contribution pondérée dans `tss_load`).
- **CTL / ATL / TSB** (modèle de Banister) : moyennes mobiles exponentielles de la charge quotidienne, constantes de 42 jours (CTL, « forme ») et 7 jours (ATL, « fatigue ») ; TSB = CTL − ATL (« fraîcheur »). La récursion est faite en Rust.
- **Jauge de forme** : le TSB est placé sur six bandes (fatigue forte < −30, modérée −30…−10, légère −10…0, frais 0…10, optimal 10…25, désentraînement > 25) avec une estimation du nombre de jours de repos pour atteindre la bande suivante.
- **Évolution et prévision** : sous la jauge, un menu déroulant « 📈 Évolution et prévision » affiche CTL, ATL et TSB (et en option le TSS quotidien) sur 14 à 180 jours (21 par défaut), avec les bandes de forme en fond. Les 7 jours suivants sont une **prévision en pointillés** : mêmes formules que le calcul réel, avec une charge nulle (aucune activité).
- **Résumé du dashboard** : la carte « 4 dernières semaines » est lue dans une ligne de cache (`dashboard_summary`), reconstruite après sync / import / changement de FTP et au premier chargement de chaque jour.

Les acronymes (TSS, IF, NP, VI, CTL, ATL, TSB, FTP, hrTSS) sont soulignés en pointillés : survol = définition, y compris dans les textes générés par le LLM.

## Segments de référence

Matching **100 % local**, indépendant des `segment_efforts` de Strava (décision volontaire).

1. Étoile tes segments sur Strava, puis `cyclai import-segments` : seules les **coordonnées** de départ / arrivée sont récupérées dans `reference_segments`. Garmin Connect n'a pas d'équivalent : sans Strava, le matching continue sur les segments déjà en base, et on ne peut pas en ajouter de nouveaux par cette commande.
2. À chaque sync, pour chaque activité, le flux GPS est comparé aux deux extrémités par distance haversine (seuil de **15 m**) ; la portion entre l'entrée et la sortie fournit puissance, vitesse et FC moyennes.
3. L'historique est traité **chronologiquement** et reconstruit à chaque sync : les deltas (vs record perso, vs moyenne des derniers passages, vs dernier passage) ne comparent un effort qu'à ce qui s'est passé *avant*.

Les efforts apparaissent sur la page de l'activité et alimentent le bilan.

## Bilan d'activité

Un bilan est un court texte qualitatif (2-4 phrases) généré **à la demande** depuis la page de l'activité — il n'est pas déclenché automatiquement par le sync, pour permettre d'ajouter du contexte avant de payer l'analyse.

- Champ **contexte libre** (`activities.user_context`) : l'intention de la séance, des sensations, la météo… Il est enregistré avec l'activité et inclus dans le prompt. Sans contexte, le modèle a pour consigne de **ne pas supposer** l'intention de la sortie.
- Entrée du prompt : profil cycliste + chiffres déjà calculés (TSS/IF, zones, D+ et ratio D+/km, VI, segments et deltas, tableau des laps s'il y en a plus d'un) — jamais l'historique brut.
- Le D+/km est fourni avec des repères plat / vallonné / montagneux, pour que la variabilité de puissance ne soit pas attribuée au terrain à tort.
- Le prompt système est modifiable dans `/settings`. Un bilan déjà généré n'est pas régénéré tant que tu ne le demandes pas.
- Fournisseur choisi dans `/settings` (**Anthropic** ou **OpenAI**). Un seul est appelé ; l'autre clé reste enregistrée.
- Anthropic : modèle fixe `claude-sonnet-5`, réflexion étendue désactivée, `max_tokens` 4096.
- OpenAI : modèle au choix parmi `gpt-6-luna` (le moins cher), `gpt-6.1-terra`, `gpt-6.1-sol` (défaut, équilibré) et `gpt-6-astra` (le plus capable). API Responses, raisonnement `low`, `max_output_tokens` 4096. La requête n'est pas conservée côté OpenAI (`store: false`).

## Digest hebdomadaire

Bouton **« Générer mon digest »** en haut de la page **Séances** (`/workouts`), avec un champ de note libre optionnel (« sortie longue prévue mardi »).

- Entrée : charge sur 28 jours, CTL/ATL/TSB, répartition des zones, note libre, profil.
- Affichage : une ligne par étape avec des symboles (🔥 échauffement, ⚡ effort, 💤 récupération, 🧊 retour au calme ; ⏱ durée, 🎯 cible, ⛰ / 〰 terrain, 📝 note) et des blocs 🔁 ×N pour les répétitions ; le texte d'analyse est réduit à 3 lignes (clic pour tout afficher).
- Sortie : un objet JSON strict `{ "workout": {...}, "notes": "..." }`. Les fences ```` ```json ```` parasites sont retirées avant parsing ; un JSON invalide est conservé en texte brut sans erreur.
- La **distance estimée** est recalculée en Rust (jamais confiée à l'arithmétique du LLM, peu fiable sur des répétitions imbriquées).
- Si la séance respecte le schéma et passe la même validation que le concepteur, elle est aussi ajoutée à `/workouts` (source « généré par digest »).
- Historique dans la table `digests` ; la page Séances affiche le dernier, avec un lien « Planifier cette séance ».

### Format d'une séance

```json
{
  "name": "Over-Under 2x12",
  "steps": [
    {"type": "warmup", "duration": {"type": "time", "value": "15min"}, "target": {"type": "power_range", "low": 123, "high": 154}},
    {"type": "repeat", "count": 2, "steps": [
      {"type": "repeat", "count": 3, "steps": [
        {"type": "interval", "duration": {"type": "time", "value": "3min"}, "target": {"type": "power_range", "low": 195, "high": 195}, "note": "sous-seuil"},
        {"type": "interval", "duration": {"type": "time", "value": "1min"}, "target": {"type": "power_range", "low": 215, "high": 220}, "note": "sur-seuil"}
      ]},
      {"type": "recovery", "duration": {"type": "time", "value": "3min"}, "target": {"type": "power_range", "low": 100, "high": 123}}
    ]},
    {"type": "cooldown", "duration": {"type": "time", "value": "10min"}, "target": {"type": "power_range", "low": 100, "high": 123}}
  ]
}
```

- Types d'étape : `warmup`, `interval`, `recovery`, `cooldown`, `repeat` (`count` + `steps`, imbriquable).
- Durées acceptées : `15min`, `90s`, `1h`, `1h30`, `1:30`, ou un nombre de minutes.
- `terrain` (optionnel) : `plat` (défaut), `vallonné`, `montée` — influence la distance estimée.
- `note` (optionnel) : texte libre par étape.
- Ce format est **stable et générique** : il est partagé par le digest, le concepteur, les séances planifiées et l'API.

## Mode sans clé API : rapport IA

Sans clé pour le **fournisseur choisi** dans `/settings`, aucun appel n'est fait et rien n'est dépensé : un bandeau l'indique (page Séances et page d'activité). Les boutons **« Télécharger le rapport IA »** (digest sur la page Séances, bilan sur la page d'activité) produisent un fichier markdown contenant la consigne, le profil, les données calculées et le format de réponse attendu — exactement ce que recevrait l'API. À coller dans Claude, ChatGPT ou autre pour obtenir la même analyse. Le même bandeau s'affiche si l'appel API échoue (clé invalide, quota, réseau).

## Concepteur de séances

`/workouts` : éditeur de séances en échelons façon TrainingPeaks — largeur = durée, hauteur = intensité cible, couleur = zone de puissance, ligne FTP. Mise à jour en direct pendant l'édition, avec durée, TSS≈ et IF≈. Le même graphique est utilisé sur la liste et sur le digest. Les blocs répétés s'imbriquent.

**Sport** d'une séance : cyclisme, course à pied ou autre. Il est choisi dans le concepteur, hérité du modèle à la planification, et montré par une icône (🚴 🏃 📅).

**Cibles** par étape, filtrées par sport :

| Type | Unité | Sports |
|---|---|---|
| Puissance (`power_range`) | W | vélo |
| Zone de puissance (`power_zone`) | 1 à 7, % du FTP | vélo |
| FC (`hr_range`) | bpm | vélo, course |
| Zone de FC (`hr_zone`) | 1 à 5 | vélo, course |
| Allure (`pace_range`) | min/km (accepte `"m:ss"`) | course |

Les cibles FC et allure sont dessinées en pointillés ; leur hauteur sur le graphique (et les TSS estimés) vient d'une **intensité équivalente** : puissance typique de la zone FC, ou allure seuil / allure de l'étape pour l'allure (l'**allure seuil** se règle dans `/settings`, 75 % par défaut si non réglée). Changer le type de cible d'une étape conserve son intensité approximative. La distance estimée d'une séance de course vient des allures des étapes ; celle du vélo, de la vitesse estimée selon la puissance et le terrain (≈ 30 km/h en Z2-Z3 sur le plat, 12-15 km/h en montée). C'est une **estimation indicative**.

## Séances planifiées et calendrier

Le calendrier du dashboard montre 6 semaines (2 passées, 4 à venir, alignées sur le lundi) : sorties réalisées en aplat (couleur = zone d'intensité, teal pour la course) et séances planifiées en contour pointillé, « ✓ » si une sortie existe déjà ce jour-là.

Une séance planifiée **possède sa propre copie des étapes** (table `planned_workouts`). Elle peut venir d'un modèle (`workout_id`, informatif) ou être unique à son jour ; modifier ou supprimer le modèle ne la change pas, et plusieurs séances par jour sont possibles.

Ajout : clic sur un jour du calendrier (`/planned/new?date=…`), bouton « Planifier » d'un modèle, champ « Planifier aussi cette séance le » du concepteur, ou l'API ([API.md](../API.md#post-apiv1planned)). Édition / suppression : clic sur la séance dans le calendrier.

## Page d'une activité

`/activity/{id}` :

- **Graphe superposé** (Chart.js) : altitude en aire grise, et jusqu'à **2** courbes choisies par menus déroulants parmi puissance, FC, vitesse (ou allure), cadence. Les échelles Y sont fixées sur l'activité entière (elles ne bougent pas au zoom) ; survol = tooltip, et un point suit le curseur sur la carte.
- **Laps** à côté du graphe : le survol prévisualise le lap (bande sur le graphe, tronçon rouge sur la carte), le clic zoome dessus et **estompe, dans les barres de zones, la part qui vient d'ailleurs que de ce lap**.
- **Carte** Leaflet avec fond OpenStreetMap.
- **Zones** de puissance (7) et de FC (5) : barres par zone (une barre pleine = max(zone la plus longue, 60 %) pour rester lisible), durées en `H:MM:SS`, plage en W / bpm au survol.
- **Courbe de puissance** : meilleures moyennes sur 5 s, 15 s, 30 s, 1, 5, 10 et 20 min.
- **Segments** détectés et leurs deltas.
- **Bilan**, titre et description, contexte libre. Un id positif ouvre la sortie sur strava.com ; une sortie Garmin ouvre Connect (`external_id`). Un import fichier n'a pas de lien.

Couleurs communes à tous les outils de l'utilisateur : FC rouge, vitesse bleu clair, cadence orange, puissance mauve, altitude gris.

## Statistiques

`/stats`, avec un filtre de période (tout, cette année, 90 jours, 30 jours) :

- **Totaux** : distance totale, à vélo, à pied, temps d'activité, dénivelé positif, nombre de sorties.
- **Distance par mois** : histogramme empilé vélo / course.
- **Faits marquants** : plus longue sortie vélo, plus longue course, plus gros dénivelé.
- **Toutes les traces** sur une carte Leaflet (vélo en couleur d'accent, course en teal) : survol = trace mise en avant avec sa date et sa distance, clic = ouvre la sortie. Les traces sont des versions simplifiées (~150 points) stockées dans `activities.route_points_json`.
- **Records sur distance** : meilleur temps et vitesse moyenne (vélo : 1, 5, 10, 20, 40, 80 km) ou allure (course : 1, 5, 10 km, semi, marathon) sur n'importe quel tronçon d'une sortie, avec la date ; une colonne « Sur la période » s'ajoute quand un filtre est actif.
- **Étoile de puissance** : radar de la meilleure puissance moyenne sur 5 s, 15 s, 30 s, 1, 5, 10, 20 et 60 min (record global, plus la période en second tracé quand un filtre est actif).

Les records sont calculés une fois par sortie (au sync, à l'import, et en rattrapage au démarrage pour l'historique) dans la table `activity_bests` : la page ne relit jamais les flux bruts.

## Itinéraire GPX

`/route` : charge un GPX d'itinéraire (trace ou `<rte>`), affiche le tracé sur la carte et le profil d'altitude **coloré par pente** (survol synchronisé avec la carte, courbe de pente optionnelle), avec distance, D+ / D−, altitude min / max et pentes maximales. L'altitude est lissée sur ~120 m et la pente estimée sur ~100 m, sinon le bruit GPS la fait osciller. Rien n'est enregistré.

## Paramètres

`/settings` regroupe tout ce qui était auparavant dans `.env`, plus l'apparence :

- **Apparence** : couleur d'accent de l'interface (9 teintes douces prédéfinies, ou une couleur personnalisée) et mode **clair / sombre / automatique** (suit le système). Les couleurs de données (zones, puissance, FC…) restent fixes. Le mode sombre s'applique aussi aux graphiques, au calendrier et aux cartes.
- **Clés API** : source de synchro (Strava ou Garmin Connect) et ses clés ; fournisseur du LLM (Anthropic ou OpenAI), clé, et modèle OpenAI.
- **Profil physiologique** : FTP, FC max, FC repos, FC seuil, coefficient hrTSS, allure seuil. Enregistrer **recalcule tout l'historique**.
- **Profil cycliste** : texte libre réinjecté dans chaque prompt.
- **Prompts système** du bilan et du digest, éditables.
