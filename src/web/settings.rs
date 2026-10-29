use super::{AppState, escape_html};
use axum::extract::State;
use axum::response::{Html, IntoResponse, Redirect, Response};
use serde::Deserialize;

/// Muted, modern accent presets (name, #rrggbb). The native color input stays
/// available for anything else.
const THEME_PRESETS: [(&str, &str); 9] = [
    ("Bleu", "#2563eb"),
    ("Indigo", "#5865c4"),
    ("Violet doux", "#7c6bc4"),
    ("Sauge", "#4f8a78"),
    ("Vert", "#3f8f5f"),
    ("Ocre", "#b9852f"),
    ("Terracotta", "#c0674b"),
    ("Rose poudré", "#be5f80"),
    ("Gris bleu", "#64748b"),
];

fn page_shell(body: &str) -> String {
    let header = super::layout::page_header(
        "Paramètres",
        "Clés, profil physiologique, détection automatique, estimation de puissance et prompts du LLM.",
        "",
    );
    let content = format!("{header}{body}");
    super::layout::Page::new("Paramètres", super::layout::Nav::Settings, &content)
        .class("narrow")
        .css("textarea.settings-textarea { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 0.82rem; min-height: 200px; width: 100%; }
input.color-input { width: 36px; height: 36px; padding: 2px; margin: 0; cursor: pointer; border-radius: 50%; }
.swatches { display: flex; flex-wrap: wrap; align-items: center; gap: 0.55rem; margin-bottom: 0.9rem; }
.swatch { width: 32px; height: 32px; min-width: 0; padding: 0; margin: 0; border-radius: 50%; border: 2px solid var(--card-bg); box-shadow: 0 0 0 1px var(--border-strong); }
.swatch:hover { transform: scale(1.1); border-color: var(--card-bg); }
.swatch.selected { box-shadow: 0 0 0 2px var(--text); }
.swatch-custom { display: inline-flex; align-items: center; gap: 0.4rem; margin-left: 0.5rem; }
.swatch-custom .hint { margin: 0; }
label.check { display: flex; align-items: center; gap: 0.5rem; color: var(--text); }
.setting-actions { display: flex; flex-wrap: wrap; gap: 0.5rem; align-items: center; }
.setting-actions form { margin: 0; }")
        .render()
}

async fn render_page(pool: &sqlx::SqlitePool, error: Option<&str>) -> Html<String> {
    let settings = match crate::settings::fetch(pool).await {
        Ok(s) => s,
        Err(err) => {
            tracing::error!(?err, "failed to load settings");
            return Html(page_shell("<p>Impossible de charger les paramètres.</p>"));
        }
    };
    let strava_refresh_token: Option<String> = sqlx::query_scalar::<_, Option<String>>(
        "SELECT strava_refresh_token FROM sync_state WHERE id = 1",
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .flatten();

    let (ftp_watts, hr_max, hr_rest, lthr, hr_tss_coef, run_pace_s): (i64, i64, i64, i64, f64, i64) = sqlx::query_as(
        "SELECT ftp_watts, hr_max, hr_rest, lthr, hr_tss_coef, run_threshold_pace_s FROM athlete_config WHERE id = 1",
    )
    .fetch_one(pool)
    .await
    .unwrap_or((0, 0, 0, 0, 1.0, 0));
    let run_pace = if run_pace_s > 0 {
        super::format::format_pace_secs(run_pace_s as f64)
    } else {
        String::new()
    };
    let (rider_kg, bike_kg, cda, crr): (f64, f64, f64, f64) = sqlx::query_as(
        "SELECT rider_weight_kg, bike_weight_kg, cda, crr FROM athlete_config WHERE id = 1",
    )
    .fetch_one(pool)
    .await
    .unwrap_or((0.0, 0.0, 0.0, 0.0));
    let auto_segments: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM reference_segments WHERE source = 'auto'",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(0);
    let auto_insights: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM activity_insights")
        .fetch_one(pool)
        .await
        .unwrap_or(0);
    let estimated_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM activities WHERE power_source = 'estimate'")
            .fetch_one(pool)
            .await
            .unwrap_or(0);

    let error_html = error
        .map(|e| format!(r#"<p class="form-error">{}</p>"#, escape_html(e)))
        .unwrap_or_default();

    let body = format!(
        r#"{error_html}
<section>
<h2>Apparence</h2>
<form method="post" action="/settings/appearance">
  <label>Couleur du thème</label>
  <div class="swatches">{swatches}<span class="swatch-custom"><input type="color" name="theme_color" value="{theme_color}" class="color-input" title="Couleur personnalisée"><span class="hint">personnalisée</span></span></div>
  <label>Mode
  <select name="theme_mode">{theme_modes}</select>
  </label>
  <script>
document.querySelectorAll('.swatch').forEach(function(b) {{
  b.addEventListener('click', function() {{
    document.querySelector('input[name=theme_color]').value = b.dataset.color;
    document.querySelectorAll('.swatch').forEach(function(x) {{ x.classList.toggle('selected', x === b); }});
  }});
}});
</script>
  <p class="hint">La couleur d'accent colore la navigation, les boutons et les liens ; les couleurs des zones et des courbes (puissance, FC…) restent fixes pour rester lisibles d'un écran à l'autre.</p>
  <button type="submit">Enregistrer</button>
  <button type="submit" name="reset" value="1" class="secondary">Réinitialiser</button>
</form>
</section>

<section>
<h2>Clés API</h2>
<form method="post" action="/settings/keys">
  <label>Source de synchronisation
  <select name="sync_source">
    <option value="strava"{strava_selected}>Strava</option>
    <option value="garmin"{garmin_selected}>Garmin Connect</option>
  </select>
  </label>
  <p class="hint">Une seule source est interrogée. Les sorties déjà en base restent. Après un changement de source, « Tout l'historique » rattrape les anciennes sorties de la nouvelle : la synchro simple ne reprend que ce qui est postérieur à la dernière synchro.</p>
  <label>Clé API Garmin Connect
  <input type="password" autocomplete="off" name="garmin_api_key" value="{garmin_api_key}">
  </label>
  <p class="hint">Clé personnelle créée sur Garmin Connect (Paramètres → Personal API). Elle sert à obtenir un jeton de 24 h à chaque synchro. Usage personnel, un seul compte. Vélo (route et home trainer) et course à pied ; le gravel, le VTT et le cardio indoor sont ignorés.</p>
  <label>Strava Client ID
  <input type="text" name="strava_client_id" value="{strava_client_id}">
  </label>
  <label>Strava Client Secret
  <input type="password" autocomplete="off" name="strava_client_secret" value="{strava_client_secret}">
  </label>
  <label>Strava Refresh Token
  <input type="password" autocomplete="off" name="strava_refresh_token" value="{strava_refresh_token}">
  </label>
  <p class="hint">Renouvelé automatiquement par Strava à chaque synchronisation — à renseigner manuellement seulement pour une première configuration ou après révocation.</p>
  <label>Fournisseur du LLM
  <select name="llm_provider">
    <option value="anthropic"{anthropic_selected}>Anthropic (Claude)</option>
    <option value="openai"{openai_selected}>OpenAI</option>
  </select>
  </label>
  <p class="hint">Un seul fournisseur est appelé pour les bilans et le digest. L'autre clé reste enregistrée.</p>
  <label>Clé API Anthropic
  <input type="password" autocomplete="off" name="anthropic_api_key" value="{anthropic_api_key}">
  </label>
  <p class="hint">Modèle fixe : Claude. Sans clé pour le fournisseur choisi, un « rapport IA » est téléchargeable depuis le dashboard et les pages d'activité.</p>
  <label>Clé API OpenAI
  <input type="password" autocomplete="off" name="openai_api_key" value="{openai_api_key}">
  </label>
  <label>Modèle OpenAI
  <select name="openai_model">{openai_models}</select>
  </label>
  <p class="hint">Utilisés seulement quand OpenAI est le fournisseur. Sol équilibre coût et qualité ; Luna est le moins cher, Astra le plus capable.</p>
  <button type="submit">Enregistrer</button>
</form>
</section>

<section>
<h2>Profil physiologique</h2>
<p class="hint">FTP recalcule rétroactivement TSS/IF/zones de tout l'historique une fois enregistrée. Les FC (max, repos, seuil) recalculent le hrTSS des courses à pied.</p>
<form method="post" action="/settings/athlete">
  <label>{ftp_label} (W)
  <input type="number" name="ftp_watts" value="{ftp_watts}" min="1">
  </label>
  <label>FC max (bpm)
  <input type="number" name="hr_max" value="{hr_max}" min="1">
  </label>
  <label>FC repos (bpm)
  <input type="number" name="hr_rest" value="{hr_rest}" min="1">
  </label>
  <label>FC seuil / LTHR (bpm, 0 = automatique)
  <input type="number" name="lthr" value="{lthr}" min="0">
  </label>
  <label>Coefficient hrTSS dans la charge totale
  <input type="number" name="hr_tss_coef" value="{hr_tss_coef}" min="0" max="10" step="0.05">
  </label>
  <p class="hint">Multiplie le hrTSS des courses quand il s'additionne au TSS puissance du vélo dans CTL/ATL/TSB et les totaux (1 = compte comme un TSS vélo, 0,8 = 20 % de moins, 0 = les courses ne comptent pas). Le hrTSS affiché sur chaque course reste le score brut.</p>
  <label>Allure seuil en course à pied (min/km, vide = non réglée)
  <input type="text" name="run_pace" value="{run_pace}" placeholder="4:30" pattern="[0-9]+:[0-5][0-9]">
  </label>
  <p class="hint">Référence des cibles d'allure du concepteur de séances : sert à placer une étape d'allure sur l'échelle d'intensité (graphique, TSS et distance estimés).</p>
  <p class="hint">Sert de référence au hrTSS (une heure à la FC seuil = 100). Automatique : FC repos + 85 % de la réserve cardiaque.</p>
  <button type="submit">Enregistrer</button>
</form>
</section>

<section>
<h2>Détection automatique</h2>
<p class="hint">Désactivée par défaut. Une fois activée, le recalcul repère les côtes faites au moins deux fois (elles deviennent des segments locaux), les blocs d'effort dans la puissance, le type de laps (split automatique ou séance) et une comparaison aux sorties récentes de durée ou de distance proche. Ces lectures sont ajoutées au bilan. Les segments importés depuis Strava ne sont pas modifiés. Décocher n'efface rien : les lectures déjà stockées restent jusqu'à « Effacer », mais elles ne sont plus injectées dans les bilans ni rematchées tant que l'option est coupée.</p>
<p>{auto_segments} segment(s) auto-détecté(s) · {auto_insights} sortie(s) avec une lecture.</p>
<div class="setting-actions">
<form method="post" action="/settings/autodetect">
  <label class="check"><input type="checkbox" name="enabled" value="1"{auto_checked}> Activer</label>
  <button type="submit">Enregistrer</button>
</form>
<form method="post" action="/settings/autodetect/recompute">
  <button type="submit" class="secondary">Recalculer</button>
</form>
<form method="post" action="/settings/autodetect/clear" onsubmit="return confirm('Effacer les segments et les lectures auto-détectés ?');">
  <button type="submit" class="secondary">Effacer</button>
</form>
</div>
</section>

<section>
<h2>Estimation de puissance</h2>
<p class="hint">Désactivée par défaut. Pour une sortie vélo sans capteur de puissance, estime les watts à partir de la vitesse et de la pente (résistance au roulement, air, gravité, accélération). Il faut le poids du cycliste, le poids du vélo, le CdA (surface de traînée : environ 0,32 en haut de cintre, 0,28 en bas) et le Crr (environ 0,004 en pneu course, 0,005 en entraînement). Les courses à pied et les sorties qui ont déjà un capteur ne sont pas modifiées. Enregistrer avec l'option activée lance le calcul. Décocher laisse les watts estimés en place jusqu'à « Effacer ».</p>
<p>{estimated_count} sortie(s) avec une puissance estimée.</p>
<form method="post" action="/settings/power">
  <label class="check"><input type="checkbox" name="enabled" value="1"{power_checked}> Activer</label>
  <label>Poids du cycliste (kg)
  <input type="text" inputmode="decimal" name="rider_weight_kg" value="{rider_weight}" placeholder="75">
  </label>
  <label>Poids du vélo (kg)
  <input type="text" inputmode="decimal" name="bike_weight_kg" value="{bike_weight}" placeholder="8">
  </label>
  <label>CdA (m²)
  <input type="text" inputmode="decimal" name="cda" value="{cda}" placeholder="0.32">
  </label>
  <label>Crr
  <input type="text" inputmode="decimal" name="crr" value="{crr}" placeholder="0.005">
  </label>
  <button type="submit">Enregistrer</button>
</form>
<div class="setting-actions">
<form method="post" action="/settings/power/recompute">
  <button type="submit" class="secondary">Recalculer</button>
</form>
<form method="post" action="/settings/power/clear" onsubmit="return confirm('Retirer la puissance estimée de toutes les sorties ?');">
  <button type="submit" class="secondary">Effacer les estimations</button>
</form>
</div>
</section>

<section>
<h2>Profil cycliste</h2>
<p class="hint">Réinjecté dans chaque prompt envoyé au LLM (bilans et digest hebdomadaire) : FTP/zones, objectifs, équipement, contraintes...</p>
<form method="post" action="/settings/profile">
  <textarea class="settings-textarea" name="athlete_profile">{athlete_profile}</textarea>
  <button type="submit">Enregistrer</button>
</form>
</section>

<section>
<h2>Prompt système — bilan d'activité</h2>
<p class="hint">Envoyé au LLM à chaque génération/régénération du bilan d'une sortie.</p>
<form method="post" action="/settings/prompts/bilan">
  <textarea class="settings-textarea" name="system_prompt">{bilan_system_prompt}</textarea>
  <button type="submit">Enregistrer</button>
</form>
</section>

<section>
<h2>Prompt système — digest hebdomadaire</h2>
<p class="hint">Envoyé au LLM à chaque génération du digest hebdomadaire (recommandation de séance).</p>
<form method="post" action="/settings/prompts/digest">
  <textarea class="settings-textarea" name="system_prompt">{digest_system_prompt}</textarea>
  <button type="submit">Enregistrer</button>
</form>
</section>
"#,
        error_html = error_html,
        theme_color = escape_html(&settings.theme_color),
        swatches = THEME_PRESETS
            .iter()
            .map(|(name, hex)| format!(
                r#"<button type="button" class="swatch{}" data-color="{hex}" style="background:{hex}" title="{name}" aria-label="{name}"></button>"#,
                if hex.eq_ignore_ascii_case(&settings.theme_color) { " selected" } else { "" }
            ))
            .collect::<String>(),
        theme_modes = super::layout::MODES
            .iter()
            .map(|(v, l)| format!(r#"<option value="{v}"{}>{l}</option>"#, if *v == settings.theme_mode { " selected" } else { "" }))
            .collect::<String>(),
        ftp_label = super::annotate_acronyms("FTP"),
        ftp_watts = ftp_watts,
        hr_max = hr_max,
        hr_rest = hr_rest,
        lthr = lthr,
        hr_tss_coef = hr_tss_coef,
        run_pace = run_pace,
        auto_checked = if settings.autodetect_enabled != 0 { " checked" } else { "" },
        auto_segments = auto_segments,
        auto_insights = auto_insights,
        power_checked = if settings.power_estimate_enabled != 0 { " checked" } else { "" },
        rider_weight = fmt_num(rider_kg),
        bike_weight = fmt_num(bike_kg),
        cda = fmt_num(cda),
        crr = fmt_num(crr),
        estimated_count = estimated_count,
        strava_selected = if settings.sync_source == "garmin" { "" } else { " selected" },
        garmin_selected = if settings.sync_source == "garmin" { " selected" } else { "" },
        garmin_api_key = escape_html(&settings.garmin_api_key),
        strava_client_id = escape_html(&settings.strava_client_id),
        strava_client_secret = escape_html(&settings.strava_client_secret),
        strava_refresh_token = escape_html(strava_refresh_token.as_deref().unwrap_or("")),
        anthropic_selected = if settings.llm_provider == "openai" { "" } else { " selected" },
        openai_selected = if settings.llm_provider == "openai" { " selected" } else { "" },
        anthropic_api_key = escape_html(&settings.anthropic_api_key),
        openai_api_key = escape_html(&settings.openai_api_key),
        openai_models = crate::llm::OPENAI_MODELS
            .iter()
            .map(|(id, label)| format!(
                r#"<option value="{id}"{}>{label}</option>"#,
                if *id == crate::llm::openai_model_or_default(&settings.openai_model) { " selected" } else { "" }
            ))
            .collect::<String>(),
        athlete_profile = escape_html(&settings.athlete_profile),
        bilan_system_prompt = escape_html(&settings.bilan_system_prompt),
        digest_system_prompt = escape_html(&settings.digest_system_prompt),
    );

    Html(page_shell(&body))
}

pub async fn page(State(state): State<AppState>) -> Html<String> {
    render_page(&state.pool, None).await
}

#[derive(Deserialize)]
pub struct KeysForm {
    #[serde(default)]
    strava_client_id: String,
    #[serde(default)]
    strava_client_secret: String,
    #[serde(default)]
    strava_refresh_token: String,
    #[serde(default)]
    anthropic_api_key: String,
    #[serde(default)]
    openai_api_key: String,
    #[serde(default)]
    llm_provider: String,
    #[serde(default)]
    openai_model: String,
    #[serde(default)]
    garmin_api_key: String,
    #[serde(default)]
    sync_source: String,
}

pub async fn update_keys(
    State(state): State<AppState>,
    axum::extract::Form(form): axum::extract::Form<KeysForm>,
) -> Redirect {
    let sync_source = if form.sync_source == "garmin" {
        "garmin"
    } else {
        "strava"
    };
    let llm_provider = if form.llm_provider == "openai" {
        "openai"
    } else {
        "anthropic"
    };
    let _ = sqlx::query(
        "UPDATE settings SET strava_client_id = ?, strava_client_secret = ?, anthropic_api_key = ?, \
         openai_api_key = ?, llm_provider = ?, openai_model = ?, garmin_api_key = ?, sync_source = ? \
         WHERE id = 1",
    )
    .bind(form.strava_client_id.trim())
    .bind(form.strava_client_secret.trim())
    .bind(form.anthropic_api_key.trim())
    .bind(form.openai_api_key.trim())
    .bind(llm_provider)
    .bind(crate::llm::openai_model_or_default(&form.openai_model))
    .bind(form.garmin_api_key.trim())
    .bind(sync_source)
    .execute(&state.pool)
    .await;

    // sync_state may not have a row yet on a fresh DB (only ever seeded/
    // written by the boot seed or a completed sync) — upsert rather than
    // assume it exists.
    let _ = sqlx::query(
        r#"
        INSERT INTO sync_state (id, last_sync_epoch, strava_refresh_token)
        VALUES (1, 0, ?)
        ON CONFLICT (id) DO UPDATE SET strava_refresh_token = excluded.strava_refresh_token
        "#,
    )
    .bind(form.strava_refresh_token.trim())
    .execute(&state.pool)
    .await;

    Redirect::to("/settings")
}

#[derive(Deserialize)]
pub struct AthleteConfigForm {
    ftp_watts: i64,
    hr_max: i64,
    hr_rest: i64,
    #[serde(default)]
    lthr: i64,
    #[serde(default = "default_hr_tss_coef")]
    hr_tss_coef: f64,
    #[serde(default)]
    run_pace: String,
}

fn default_hr_tss_coef() -> f64 {
    1.0
}

/// Updates FTP/HR max/HR rest, then immediately re-runs the deterministic
/// metrics pipeline so TSS/IF/zones retroactively reflect the new FTP across
/// the whole history — matches the sync-time behavior in metrics::run.
pub async fn update_athlete_config(
    State(state): State<AppState>,
    axum::extract::Form(form): axum::extract::Form<AthleteConfigForm>,
) -> Redirect {
    let _ = sqlx::query(
        "UPDATE athlete_config SET ftp_watts = ?, hr_max = ?, hr_rest = ?, lthr = ?, hr_tss_coef = ?, run_threshold_pace_s = ?, updated_at = datetime('now') WHERE id = 1",
    )
    .bind(form.ftp_watts)
    .bind(form.hr_max)
    .bind(form.hr_rest)
    .bind(form.lthr.max(0))
    .bind(if form.hr_tss_coef.is_finite() { form.hr_tss_coef.clamp(0.0, 10.0) } else { 1.0 })
    // Blank or unreadable = not set (0).
    .bind(crate::workout::parse_pace(&form.run_pace).filter(|p| *p > 0.0).map(|p| p.round() as i64).unwrap_or(0))
    .execute(&state.pool)
    .await;

    let _ = crate::metrics::run(&state.pool).await;

    Redirect::to("/settings")
}

#[derive(Deserialize)]
pub struct AppearanceForm {
    #[serde(default)]
    theme_color: String,
    #[serde(default)]
    theme_mode: String,
    #[serde(default)]
    reset: String,
}

pub async fn update_appearance(
    State(state): State<AppState>,
    axum::extract::Form(form): axum::extract::Form<AppearanceForm>,
) -> Redirect {
    let (color, mode) = if !form.reset.is_empty() {
        (
            super::layout::DEFAULT_ACCENT.to_string(),
            "auto".to_string(),
        )
    } else {
        let color = form.theme_color.trim().to_lowercase();
        (
            if super::layout::valid_color(&color) {
                color
            } else {
                super::layout::DEFAULT_ACCENT.to_string()
            },
            if super::layout::MODES
                .iter()
                .any(|(m, _)| *m == form.theme_mode)
            {
                form.theme_mode
            } else {
                "auto".to_string()
            },
        )
    };
    let _ = sqlx::query("UPDATE settings SET theme_color = ?, theme_mode = ? WHERE id = 1")
        .bind(&color)
        .bind(&mode)
        .execute(&state.pool)
        .await;
    super::layout::set_theme(&color, &mode);
    Redirect::to("/settings")
}

#[derive(Deserialize)]
pub struct ProfileForm {
    #[serde(default)]
    athlete_profile: String,
}

pub async fn update_profile(
    State(state): State<AppState>,
    axum::extract::Form(form): axum::extract::Form<ProfileForm>,
) -> Redirect {
    let _ = sqlx::query("UPDATE settings SET athlete_profile = ? WHERE id = 1")
        .bind(&form.athlete_profile)
        .execute(&state.pool)
        .await;
    Redirect::to("/settings")
}

#[derive(Deserialize)]
pub struct PromptForm {
    system_prompt: String,
}

pub async fn update_bilan_prompt(
    State(state): State<AppState>,
    axum::extract::Form(form): axum::extract::Form<PromptForm>,
) -> Redirect {
    let _ = sqlx::query("UPDATE settings SET bilan_system_prompt = ? WHERE id = 1")
        .bind(&form.system_prompt)
        .execute(&state.pool)
        .await;
    Redirect::to("/settings")
}

pub async fn update_digest_prompt(
    State(state): State<AppState>,
    axum::extract::Form(form): axum::extract::Form<PromptForm>,
) -> Redirect {
    let _ = sqlx::query("UPDATE settings SET digest_system_prompt = ? WHERE id = 1")
        .bind(&form.system_prompt)
        .execute(&state.pool)
        .await;
    Redirect::to("/settings")
}

fn fmt_num(v: f64) -> String {
    if v <= 0.0 || !v.is_finite() {
        return String::new();
    }
    let s = format!("{v:.4}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn parse_num(raw: &str) -> f64 {
    let v: f64 = raw.trim().replace(',', ".").parse().unwrap_or(0.0);
    if v.is_finite() { v } else { 0.0 }
}

#[derive(Deserialize)]
pub struct ToggleForm {
    #[serde(default)]
    enabled: String,
}

pub async fn update_autodetect(
    State(state): State<AppState>,
    axum::extract::Form(form): axum::extract::Form<ToggleForm>,
) -> Redirect {
    let _ = sqlx::query("UPDATE settings SET autodetect_enabled = ? WHERE id = 1")
        .bind(if form.enabled == "1" { 1 } else { 0 })
        .execute(&state.pool)
        .await;
    Redirect::to("/settings")
}

pub async fn recompute_autodetect(State(state): State<AppState>) -> Response {
    let enabled = crate::settings::fetch(&state.pool)
        .await
        .map(|s| s.autodetect_enabled != 0)
        .unwrap_or(false);
    if !enabled {
        return Html(
            render_page(
                &state.pool,
                Some("Activez la détection automatique avant de recalculer."),
            )
            .await,
        )
        .into_response();
    }
    match crate::autodetect::recompute(&state.pool).await {
        Ok(stats) => {
            tracing::info!(
                segments = stats.segments,
                insights = stats.insights,
                "autodetect recomputed"
            );
            Redirect::to("/settings").into_response()
        }
        Err(err) => {
            tracing::error!(?err, "autodetect recompute failed");
            Html(render_page(&state.pool, Some("Le recalcul de la détection a échoué.")).await)
                .into_response()
        }
    }
}

pub async fn clear_autodetect(State(state): State<AppState>) -> Response {
    match crate::autodetect::clear(&state.pool).await {
        Ok(()) => Redirect::to("/settings").into_response(),
        Err(err) => {
            tracing::error!(?err, "autodetect clear failed");
            Html(
                render_page(
                    &state.pool,
                    Some("Impossible d'effacer la détection automatique."),
                )
                .await,
            )
            .into_response()
        }
    }
}

#[derive(Deserialize)]
pub struct PowerForm {
    #[serde(default)]
    enabled: String,
    #[serde(default)]
    rider_weight_kg: String,
    #[serde(default)]
    bike_weight_kg: String,
    #[serde(default)]
    cda: String,
    #[serde(default)]
    crr: String,
}

fn model_from_form(form: &PowerForm) -> crate::power_estimate::Model {
    crate::power_estimate::Model {
        rider_kg: parse_num(&form.rider_weight_kg),
        bike_kg: parse_num(&form.bike_weight_kg),
        cda: parse_num(&form.cda),
        crr: parse_num(&form.crr),
    }
}

async fn save_power_model(
    pool: &sqlx::SqlitePool,
    model: crate::power_estimate::Model,
    enabled: bool,
) {
    let _ = sqlx::query(
        "UPDATE athlete_config SET rider_weight_kg = ?, bike_weight_kg = ?, cda = ?, crr = ?, updated_at = datetime('now') WHERE id = 1",
    )
    .bind(model.rider_kg)
    .bind(model.bike_kg)
    .bind(model.cda)
    .bind(model.crr)
    .execute(pool)
    .await;
    let _ = sqlx::query("UPDATE settings SET power_estimate_enabled = ? WHERE id = 1")
        .bind(if enabled { 1 } else { 0 })
        .execute(pool)
        .await;
}

async fn run_power_estimate(pool: &sqlx::SqlitePool) -> anyhow::Result<()> {
    crate::power_estimate::apply_all(pool).await?;
    crate::metrics::run(pool).await?;
    crate::segments::run(pool).await?;
    Ok(())
}

pub async fn update_power_estimate(
    State(state): State<AppState>,
    axum::extract::Form(form): axum::extract::Form<PowerForm>,
) -> Response {
    let model = model_from_form(&form);
    let want = form.enabled == "1";
    if want {
        if let Some(reason) = model.reject_reason() {
            save_power_model(&state.pool, model, false).await;
            return Html(
                render_page(
                    &state.pool,
                    Some(&format!("Estimation non activée : {reason}.")),
                )
                .await,
            )
            .into_response();
        }
    }
    save_power_model(&state.pool, model, want).await;
    if want {
        if let Err(err) = run_power_estimate(&state.pool).await {
            tracing::error!(?err, "power estimate failed");
            return Html(
                render_page(&state.pool, Some("L'estimation de puissance a échoué.")).await,
            )
            .into_response();
        }
    }
    Redirect::to("/settings").into_response()
}

pub async fn recompute_power_estimate(State(state): State<AppState>) -> Response {
    let enabled = crate::settings::fetch(&state.pool)
        .await
        .map(|s| s.power_estimate_enabled != 0)
        .unwrap_or(false);
    if !enabled {
        return Html(
            render_page(
                &state.pool,
                Some("Activez l'estimation de puissance avant de recalculer."),
            )
            .await,
        )
        .into_response();
    }
    let model = crate::power_estimate::load_model(&state.pool).await.ok();
    if model.is_none_or(|m| m.reject_reason().is_some()) {
        return Html(
            render_page(
                &state.pool,
                Some("Renseignez le poids, le CdA et le Crr avant de recalculer."),
            )
            .await,
        )
        .into_response();
    }
    match run_power_estimate(&state.pool).await {
        Ok(()) => Redirect::to("/settings").into_response(),
        Err(err) => {
            tracing::error!(?err, "power estimate recompute failed");
            Html(
                render_page(
                    &state.pool,
                    Some("Le recalcul de la puissance estimée a échoué."),
                )
                .await,
            )
            .into_response()
        }
    }
}

pub async fn clear_power_estimate(State(state): State<AppState>) -> Response {
    if let Err(err) = crate::power_estimate::clear(&state.pool).await {
        tracing::error!(?err, "power estimate clear failed");
        return Html(
            render_page(
                &state.pool,
                Some("Impossible d'effacer la puissance estimée."),
            )
            .await,
        )
        .into_response();
    }
    let _ = crate::metrics::run(&state.pool).await;
    let _ = crate::segments::run(&state.pool).await;
    Redirect::to("/settings").into_response()
}
