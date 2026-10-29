use super::{escape_html, AppState};
use axum::extract::State;
use axum::response::{Html, Redirect};
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
    let header = super::layout::page_header("Paramètres", "Clés, profil physiologique, profil cycliste et prompts du LLM.", "");
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
.swatch-custom .hint { margin: 0; }")
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
    let strava_refresh_token: Option<String> =
        sqlx::query_scalar::<_, Option<String>>("SELECT strava_refresh_token FROM sync_state WHERE id = 1")
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
    let run_pace = if run_pace_s > 0 { super::format::format_pace_secs(run_pace_s as f64) } else { String::new() };

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
  <label>Clé API Anthropic
  <input type="password" autocomplete="off" name="anthropic_api_key" value="{anthropic_api_key}">
  </label>
  <p class="hint">Optionnelle : sans clé, les bilans/digests ne sont pas générés automatiquement, mais un « rapport IA » est téléchargeable depuis le dashboard et les pages d'activité pour le coller dans Claude, ChatGPT…</p>
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
        strava_selected = if settings.sync_source == "garmin" { "" } else { " selected" },
        garmin_selected = if settings.sync_source == "garmin" { " selected" } else { "" },
        garmin_api_key = escape_html(&settings.garmin_api_key),
        strava_client_id = escape_html(&settings.strava_client_id),
        strava_client_secret = escape_html(&settings.strava_client_secret),
        strava_refresh_token = escape_html(strava_refresh_token.as_deref().unwrap_or("")),
        anthropic_api_key = escape_html(&settings.anthropic_api_key),
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
    garmin_api_key: String,
    #[serde(default)]
    sync_source: String,
}

pub async fn update_keys(State(state): State<AppState>, axum::extract::Form(form): axum::extract::Form<KeysForm>) -> Redirect {
    let sync_source = if form.sync_source == "garmin" { "garmin" } else { "strava" };
    let _ = sqlx::query(
        "UPDATE settings SET strava_client_id = ?, strava_client_secret = ?, anthropic_api_key = ?, \
         garmin_api_key = ?, sync_source = ? WHERE id = 1",
    )
    .bind(form.strava_client_id.trim())
    .bind(form.strava_client_secret.trim())
    .bind(form.anthropic_api_key.trim())
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

pub async fn update_appearance(State(state): State<AppState>, axum::extract::Form(form): axum::extract::Form<AppearanceForm>) -> Redirect {
    let (color, mode) = if !form.reset.is_empty() {
        (super::layout::DEFAULT_ACCENT.to_string(), "auto".to_string())
    } else {
        let color = form.theme_color.trim().to_lowercase();
        (
            if super::layout::valid_color(&color) { color } else { super::layout::DEFAULT_ACCENT.to_string() },
            if super::layout::MODES.iter().any(|(m, _)| *m == form.theme_mode) { form.theme_mode } else { "auto".to_string() },
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

pub async fn update_profile(State(state): State<AppState>, axum::extract::Form(form): axum::extract::Form<ProfileForm>) -> Redirect {
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

pub async fn update_bilan_prompt(State(state): State<AppState>, axum::extract::Form(form): axum::extract::Form<PromptForm>) -> Redirect {
    let _ = sqlx::query("UPDATE settings SET bilan_system_prompt = ? WHERE id = 1")
        .bind(&form.system_prompt)
        .execute(&state.pool)
        .await;
    Redirect::to("/settings")
}

pub async fn update_digest_prompt(State(state): State<AppState>, axum::extract::Form(form): axum::extract::Form<PromptForm>) -> Redirect {
    let _ = sqlx::query("UPDATE settings SET digest_system_prompt = ? WHERE id = 1")
        .bind(&form.system_prompt)
        .execute(&state.pool)
        .await;
    Redirect::to("/settings")
}
