//! Shared page shell (head, top navigation, container) and the one base
//! stylesheet every page builds on — so all pages get the same typography,
//! form controls, buttons, cards and alerts instead of each one carrying its
//! own copy. Page-specific CSS / `<head>` additions are passed to [`Page`].

use std::sync::RwLock;

/// User-chosen appearance (`/settings` → Apparence), kept in memory so every
/// page render can read it without touching the database.
struct Theme {
    color: String,
    mode: String,
}

static THEME: RwLock<Theme> = RwLock::new(Theme { color: String::new(), mode: String::new() });

pub(super) const DEFAULT_ACCENT: &str = "#2563eb";
pub(super) const MODES: [(&str, &str); 3] = [("auto", "Automatique (système)"), ("light", "Clair"), ("dark", "Sombre")];

/// `#rrggbb` only — the value is injected into a stylesheet.
pub(super) fn valid_color(c: &str) -> bool {
    c.len() == 7 && c.starts_with('#') && c[1..].chars().all(|ch| ch.is_ascii_hexdigit())
}

pub(crate) fn set_theme(color: &str, mode: &str) {
    let color = if valid_color(color) { color } else { DEFAULT_ACCENT };
    let mode = if MODES.iter().any(|(m, _)| *m == mode) { mode } else { "auto" };
    if let Ok(mut t) = THEME.write() {
        t.color = color.to_string();
        t.mode = mode.to_string();
    }
}

fn current_theme() -> (String, String) {
    match THEME.read() {
        Ok(t) if !t.color.is_empty() => (t.color.clone(), t.mode.clone()),
        _ => (DEFAULT_ACCENT.to_string(), "auto".to_string()),
    }
}

/// Black or white, whichever reads better on top of `hex` (WCAG luminance).
fn on_color(hex: &str) -> &'static str {
    let ch = |i: usize| {
        let v = u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0) as f64 / 255.0;
        if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    let lum = 0.2126 * ch(1) + 0.7152 * ch(3) + 0.0722 * ch(5);
    if lum > 0.42 { "#111827" } else { "#ffffff" }
}

const DARK_TOKENS: &str = r#"
  color-scheme: dark;
  --bg: #0e1218;
  --card-bg: #161b24;
  --text: #e6e9ef;
  --text-muted: #9aa4b5;
  --text-soft: #b6bfcd;
  --text-faint: #6b7587;
  --border: #272e3b;
  --border-strong: #3a4356;
  --input-bg: #0e1218;
  --topbar-bg: rgba(22, 27, 36, 0.92);
  --danger-text: #fca5a5;
  --danger-soft: #3b1618;
  --danger-border: #7f1d1d;
  --warn-soft: #36270b;
  --warn-border: #8a5a12;
  --warn-text: #fcd34d;
  --shadow: 0 1px 2px rgba(0, 0, 0, 0.4);
  --shadow-hover: 0 4px 14px rgba(0, 0, 0, 0.5);
  --focus: 0 0 0 3px rgba(147, 197, 253, 0.3);
"#;

/// Accent color (and derived shades) plus the dark palette, placed after the
/// base stylesheet so it overrides its light defaults.
fn theme_css(color: &str) -> String {
    format!(
        r#"
:root {{
  --accent: {color};
  --on-accent: {on};
  --accent-hover: color-mix(in srgb, var(--accent) 82%, black);
  --accent-soft: color-mix(in srgb, var(--accent) 11%, var(--card-bg));
}}
:root[data-theme="dark"] {{{DARK_TOKENS}}}
@media (prefers-color-scheme: dark) {{ :root[data-theme="auto"] {{{DARK_TOKENS}}} }}
"#,
        on = on_color(color),
    )
}

/// Runs in `<head>` after the page's scripts: exposes CSS variables to JS
/// (canvas / Leaflet can't read `var(--x)`) and themes Chart.js and maps.
const THEME_JS: &str = r#"<script>
window.cssVar = function(n) { return getComputedStyle(document.documentElement).getPropertyValue(n).trim(); };
window.isDarkTheme = function() { return getComputedStyle(document.documentElement).colorScheme === 'dark'; };
if (window.Chart) { Chart.defaults.color = cssVar('--text-muted'); Chart.defaults.borderColor = cssVar('--border'); }
document.addEventListener('DOMContentLoaded', function() { if (window.isDarkTheme()) document.body.classList.add('map-dark'); });
</script>"#;

/// Which top-navigation entry is highlighted.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Nav {
    Home,
    Workouts,
    Stats,
    Segments,
    Route,
    Settings,
}

const NAV_ITEMS: [(Nav, &str, &str); 6] = [
    (Nav::Home, "/", "Tableau de bord"),
    (Nav::Workouts, "/workouts", "Séances"),
    (Nav::Stats, "/stats", "Statistiques"),
    (Nav::Segments, "/segments", "Segments"),
    (Nav::Route, "/route", "Itinéraire"),
    (Nav::Settings, "/settings", "Paramètres"),
];

pub(super) struct Page<'a> {
    title: &'a str,
    nav: Nav,
    /// Extra class(es) on the `<main class="container …">` element:
    /// `narrow` (forms), `wide` (data-heavy pages).
    class: &'a str,
    head: &'a str,
    css: &'a str,
    body: &'a str,
}

impl<'a> Page<'a> {
    pub(super) fn new(title: &'a str, nav: Nav, body: &'a str) -> Self {
        Self { title, nav, class: "", head: "", css: "", body }
    }

    pub(super) fn class(mut self, class: &'a str) -> Self {
        self.class = class;
        self
    }

    /// Extra `<head>` markup (scripts, stylesheets).
    pub(super) fn head(mut self, head: &'a str) -> Self {
        self.head = head;
        self
    }

    /// Page-specific CSS, appended after the base stylesheet.
    pub(super) fn css(mut self, css: &'a str) -> Self {
        self.css = css;
        self
    }

    pub(super) fn render(self) -> String {
        let nav: String = NAV_ITEMS
            .iter()
            .map(|(item, href, label)| {
                let current = if *item == self.nav { r#" class="active" aria-current="page""# } else { "" };
                format!(r#"<a href="{href}"{current}>{label}</a>"#)
            })
            .collect();
        let title = if self.nav == Nav::Home && self.title.is_empty() {
            "CyclAI".to_string()
        } else {
            format!("{} · CyclAI", self.title)
        };
        let (accent, mode) = current_theme();
        format!(
            r##"<!doctype html>
<html lang="fr" data-theme="{mode}">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<link rel="icon" href="/favicon.ico" sizes="any">
<link rel="manifest" href="/manifest.webmanifest">
<link rel="apple-touch-icon" href="/apple-touch-icon.png">
<meta name="theme-color" content="#0e1218">
<script>if('serviceWorker' in navigator)navigator.serviceWorker.register('/sw.js');</script>
<style>{base_css}{theme_css}{css}</style>
{head}
{theme_js}
</head>
<body>
<header class="topbar">
<div class="topbar-inner">
<a class="brand" href="/"><span class="brand-mark" aria-hidden="true"></span>CyclAI</a>
<nav class="nav" aria-label="Navigation principale">{nav}</nav>
</div>
</header>
<main class="container {class}">
{body}
</main>
</body>
</html>"##,
            title = super::escape_html(&title),
            base_css = BASE_CSS,
            theme_css = theme_css(&accent),
            theme_js = THEME_JS,
            mode = mode,
            css = self.css,
            head = self.head,
            class = self.class,
            body = self.body,
        )
    }
}

/// `<div class="page-header">` with a title, optional subtitle and actions.
pub(super) fn page_header(title: &str, subtitle: &str, actions_html: &str) -> String {
    let subtitle = if subtitle.is_empty() {
        String::new()
    } else {
        format!(r#"<p class="subtitle">{subtitle}</p>"#)
    };
    format!(
        r#"<div class="page-header"><div><h1>{title}</h1>{subtitle}</div><div class="page-actions">{actions_html}</div></div>"#
    )
}

pub(super) const BASE_CSS: &str = r##"
:root {
  color-scheme: light;
  --bg: #f4f5f8;
  --card-bg: #ffffff;
  --text: #151a26;
  --text-muted: #667085;
  --border: #e4e7ec;
  --border-strong: #d0d5dd;
  --input-bg: #ffffff;
  --text-soft: #475467;
  --text-faint: #98a2b3;
  --topbar-bg: rgba(255, 255, 255, 0.92);
  --danger: #dc2626;
  --danger-hover: #b91c1c;
  --danger-text: #b91c1c;
  --danger-soft: #fef2f2;
  --danger-border: #fecaca;
  --warn-soft: #fffbeb;
  --warn-border: #f5c26b;
  --warn-text: #92400e;
  --radius: 12px;
  --radius-sm: 8px;
  --shadow: 0 1px 2px rgba(16, 24, 40, 0.05), 0 1px 3px rgba(16, 24, 40, 0.04);
  --shadow-hover: 0 4px 12px rgba(16, 24, 40, 0.09);
  --focus: 0 0 0 3px rgba(37, 99, 235, 0.25);
}
* { box-sizing: border-box; }
html { -webkit-text-size-adjust: 100%; }
body {
  font-family: system-ui, -apple-system, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif;
  background: var(--bg);
  color: var(--text);
  margin: 0;
  line-height: 1.5;
  font-size: 15px;
  font-variant-numeric: tabular-nums;
}
a { color: var(--accent); text-decoration: none; }
a:hover { text-decoration: underline; }
h1 { font-size: 1.6rem; line-height: 1.2; margin: 0; letter-spacing: -0.01em; }
h2 { font-size: 1.05rem; margin: 0 0 1rem; letter-spacing: -0.005em; }
h3 { font-size: 0.95rem; margin: 0.6rem 0; }
:focus-visible { outline: none; box-shadow: var(--focus); border-radius: 6px; }

/* ---- Shell ---- */
.topbar { position: sticky; top: 0; z-index: 40; background: var(--topbar-bg); backdrop-filter: saturate(1.4) blur(8px); border-bottom: 1px solid var(--border); }
.topbar-inner { max-width: 1240px; margin: 0 auto; padding: 0 1rem; min-height: 54px; display: flex; align-items: center; gap: 1.5rem; flex-wrap: wrap; }
.brand { display: inline-flex; align-items: center; gap: 0.55rem; font-weight: 700; font-size: 1.05rem; color: var(--text); letter-spacing: -0.01em; }
.brand:hover { text-decoration: none; }
.brand-mark { width: 22px; height: 22px; border-radius: 6px; background: var(--accent) url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 22 22'%3E%3Cpath d='M4 15l4-5 3.5 3 6.5-8' fill='none' stroke='white' stroke-width='2.2' stroke-linecap='round' stroke-linejoin='round'/%3E%3C/svg%3E") center / 100% no-repeat; }
.nav { display: flex; gap: 0.25rem; flex-wrap: wrap; }
.nav a { color: var(--text-muted); font-size: 0.9rem; font-weight: 500; padding: 0.4rem 0.75rem; border-radius: 999px; }
.nav a:hover { color: var(--text); background: var(--bg); text-decoration: none; }
.nav a.active { color: var(--accent); background: var(--accent-soft); }
.container { max-width: 1080px; margin: 0 auto; padding: 1.75rem 1rem 4rem; }
.container.narrow { max-width: 760px; }
.container.wide { max-width: 1240px; }
.page-header { display: flex; flex-wrap: wrap; justify-content: space-between; align-items: flex-end; gap: 0.75rem 1.5rem; margin-bottom: 1.5rem; }
.page-header .subtitle { margin: 0.3rem 0 0; color: var(--text-muted); font-size: 0.9rem; }
.page-actions { display: flex; flex-wrap: wrap; align-items: center; gap: 0.5rem 0.75rem; }
.back-link { display: inline-flex; align-items: center; gap: 0.3rem; margin-bottom: 0.75rem; color: var(--text-muted); font-size: 0.88rem; }
.back-link:hover { color: var(--accent); text-decoration: none; }

/* ---- Cards ---- */
section, .card {
  background: var(--card-bg);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  padding: 1.25rem 1.4rem;
  margin-bottom: 1.25rem;
  box-shadow: var(--shadow);
}
section > h2:first-child, .card > h2:first-child { padding-bottom: 0.7rem; border-bottom: 1px solid var(--border); margin-bottom: 1rem; }
.card-head { display: flex; justify-content: space-between; align-items: center; gap: 0.75rem; margin-bottom: 1rem; }
.card-head h2 { margin: 0; padding: 0; border: 0; }

/* ---- Forms ---- */
label { font-size: 0.85rem; color: var(--text-muted); display: block; margin-bottom: 0.7rem; font-weight: 500; }
input[type=number], input[type=text], input[type=date], input[type=password], select, textarea {
  border: 1px solid var(--border-strong);
  border-radius: var(--radius-sm);
  padding: 0.5rem 0.7rem;
  font-size: 0.9rem;
  font-family: inherit;
  color: var(--text);
  background: var(--input-bg);
  width: 100%;
  max-width: 420px;
  margin-top: 0.3rem;
  transition: border-color 0.12s, box-shadow 0.12s;
}
select { width: auto; max-width: 100%; }
textarea { max-width: 100%; }
input:focus, select:focus, textarea:focus { outline: none; border-color: var(--accent); box-shadow: var(--focus); }
input[type=file] { font-size: 0.85rem; max-width: 100%; color: var(--text-muted); }
input[type=file]::file-selector-button { border: 1px solid var(--border-strong); background: var(--input-bg); border-radius: var(--radius-sm); padding: 0.4rem 0.8rem; margin-right: 0.7rem; font-family: inherit; font-size: 0.85rem; color: var(--text); cursor: pointer; }
input[type=file]::file-selector-button:hover { background: var(--bg); }
.inline-check { display: flex; align-items: center; gap: 0.5rem; }
.hint { color: var(--text-muted); font-size: 0.85rem; margin: 0.4rem 0 0; }

/* ---- Buttons ---- */
button, .btn {
  display: inline-flex; align-items: center; justify-content: center; gap: 0.4rem;
  background: var(--accent);
  color: var(--on-accent);
  border: 1px solid var(--accent);
  border-radius: var(--radius-sm);
  padding: 0.5rem 1.05rem;
  font-size: 0.85rem;
  font-weight: 500;
  font-family: inherit;
  line-height: 1.3;
  cursor: pointer;
  margin-top: 0.65rem;
  transition: background 0.12s, border-color 0.12s, box-shadow 0.12s;
}
button:hover, .btn:hover { background: var(--accent-hover); border-color: var(--accent-hover); text-decoration: none; }
button:disabled { opacity: 0.5; cursor: not-allowed; }
button.secondary, .btn.secondary { background: var(--input-bg); color: var(--accent); border-color: var(--border-strong); margin-left: 0; }
button.secondary:hover, .btn.secondary:hover { background: var(--accent-soft); border-color: var(--accent); }
button.danger { background: var(--danger); border-color: var(--danger); color: #fff; }
button.danger:hover { background: var(--danger-hover); border-color: var(--danger-hover); }
form > button + button, form > .btn + button, form > button + .btn { margin-left: 0.4rem; }
.toolbar { display: flex; flex-wrap: wrap; align-items: center; gap: 0.6rem 1rem; }
.toolbar form { display: flex; flex-wrap: wrap; align-items: center; gap: 0.5rem; margin: 0; }
.toolbar button { margin-top: 0; margin-left: 0; }
.toolbar input[type=file] { max-width: 220px; }
.page-actions form { display: flex; flex-wrap: wrap; align-items: center; gap: 0.5rem; margin: 0; }
.page-actions button { margin: 0; }

/* ---- Alerts ---- */
.alert, .warn-banner, .form-error { border-radius: var(--radius-sm); padding: 0.65rem 0.9rem; font-size: 0.88rem; margin: 0 0 1rem; border: 1px solid; }
.warn-banner { background: var(--warn-soft); border-color: var(--warn-border); color: var(--warn-text); }
.warn-banner a { color: inherit; font-weight: 600; text-decoration: underline; }
.form-error { background: var(--danger-soft); border-color: var(--danger-border); color: var(--danger-text); }
.form-error-inline { color: var(--danger-text); }

/* ---- Stat tiles ---- */
.stats { display: grid; grid-template-columns: repeat(auto-fit, minmax(118px, 1fr)); gap: 0.65rem; margin-bottom: 1rem; }
.stat { background: var(--bg); border-radius: var(--radius-sm); padding: 0.65rem 0.8rem; }
.stats .stat .value { font-size: 1.3rem; font-weight: 650; line-height: 1.25; }
.stats .stat .label { font-size: 0.7rem; color: var(--text-muted); text-transform: uppercase; letter-spacing: 0.04em; margin-top: 0.1rem; }

.container > .stats .stat { background: var(--card-bg); border: 1px solid var(--border); box-shadow: var(--shadow); }

/* ---- Dashboard ---- */
.dash-grid { display: grid; gap: 1.25rem; grid-template-columns: minmax(0, 1fr); align-items: start; }
.dash-grid > div { min-width: 0; }
.dash-side { order: -1; }
@media (min-width: 1100px) {
  .dash-grid { grid-template-columns: minmax(0, 1fr) 400px; }
  .dash-side { order: 0; position: sticky; top: 70px; }
}
.activity-list { list-style: none; padding: 0; margin: 0; display: flex; flex-direction: column; gap: 0.7rem; }
.activity-card { display: flex; gap: 1rem; padding: 0.8rem; border: 1px solid var(--border); border-radius: var(--radius-sm); align-items: flex-start; background: var(--card-bg); transition: box-shadow 0.15s, border-color 0.15s; }
.activity-card:hover { box-shadow: var(--shadow-hover); border-color: var(--border-strong); }
.route-thumb { flex-shrink: 0; background: var(--accent-soft); border-radius: 6px; }
.route-thumb-empty { opacity: 0.5; }
.activity-body { flex: 1; min-width: 0; }
.activity-meta { font-size: 0.88rem; font-weight: 600; margin-bottom: 0.3rem; }
.activity-meta a:first-child { color: var(--text); }
.activity-meta a:first-child:hover { color: var(--accent); }
.activity-meta a:not(:first-child) { font-weight: 400; font-size: 0.8rem; }
.activity-stats { flex-shrink: 0; display: flex; flex-direction: column; gap: 0.15rem; font-size: 0.8rem; color: var(--text-muted); text-align: right; min-width: 90px; }
@media (max-width: 640px) {
  .activity-card { flex-wrap: wrap; }
  .activity-body { flex-basis: calc(100% - 112px); }
  .activity-stats { flex-direction: row; flex-wrap: wrap; gap: 0.2rem 0.9rem; text-align: left; min-width: 0; width: 100%; }
}
.bilan { font-style: italic; color: var(--text-soft); font-size: 0.88rem; margin: 0; }
.activity-body .bilan { display: -webkit-box; -webkit-box-orient: vertical; -webkit-line-clamp: 3; line-clamp: 3; overflow: hidden; cursor: pointer; }
.activity-body .bilan.expanded { display: block; -webkit-line-clamp: unset; line-clamp: unset; }
.activity-body .bilan.pending { cursor: auto; }
.bilan.pending { color: var(--text-faint); font-style: normal; }
.digest-date { font-size: 0.8rem; color: var(--text-muted); margin: 0.75rem 0; }
.steps { list-style: none; padding: 0; margin: 0.3rem 0; }
.steps .steps { margin: 0.2rem 0 0.2rem 0.3rem; padding-left: 0.7rem; border-left: 2px solid var(--border-strong); }
.step { display: flex; flex-wrap: wrap; align-items: center; gap: 0.3rem 0.45rem; padding: 0.3rem 0.55rem; margin: 0.25rem 0; background: var(--bg); border-radius: var(--radius-sm); border-left: 3px solid var(--border-strong); font-size: 0.85rem; }
.step-warmup { border-left-color: #3b82f6; }
.step-interval { border-left-color: #f97316; }
.step-recovery { border-left-color: #22c55e; }
.step-cooldown { border-left-color: #9ca3af; }
.step-repeat { display: block; background: none; padding: 0.15rem 0 0.15rem 0.1rem; border-left: 0; }
.step-repeat > .step-kind { font-weight: 650; color: var(--accent); }
.step-kind { font-weight: 600; min-width: 8.5em; }
.chip { background: var(--card-bg); border: 1px solid var(--border); border-radius: 999px; padding: 0 0.55rem; font-size: 0.8rem; white-space: nowrap; }
.chip-target { font-weight: 600; }
.step-note { color: var(--text-muted); font-size: 0.8rem; font-style: italic; }
.clamp-3 { display: -webkit-box; -webkit-box-orient: vertical; -webkit-line-clamp: 3; line-clamp: 3; overflow: hidden; cursor: pointer; }
.clamp-3.expanded { display: block; }

.load-chart { margin: 0.2rem 0 1.1rem; }
.load-chart summary { cursor: pointer; text-align: center; color: var(--text-muted); padding: 0.1rem 0; list-style: none; line-height: 1; }
.load-chart summary::-webkit-details-marker { display: none; }
.load-chart summary::after { content: ''; display: inline-block; width: 0.55rem; height: 0.55rem; border-right: 2px solid currentColor; border-bottom: 2px solid currentColor; transform: rotate(45deg); transition: transform 0.15s; margin: 0.25rem 0 0.35rem; }
.load-chart summary:hover { color: var(--accent); }
.load-chart[open] summary::after { transform: rotate(-135deg); margin: 0.5rem 0 0.1rem; }
.load-chart-controls { display: flex; flex-wrap: wrap; align-items: center; gap: 0.3rem 1rem; margin: 0.4rem 0; font-size: 0.82rem; }
.load-chart-controls label { margin: 0; display: inline-flex; align-items: center; gap: 0.35rem; }
.load-chart-controls select { margin: 0; padding: 0.25rem 0.5rem; }
.load-chart-wrap { height: 260px; }

/* ---- Zone bars ---- */
.zone-rows { display: grid; grid-template-columns: minmax(120px, auto) 1fr auto; row-gap: 0.4rem; column-gap: 0.7rem; align-items: center; font-size: 0.8rem; }
.zone-row { display: contents; }
.zone-row-label { color: var(--text-muted); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.zone-row-bar-wrap { background: var(--bg); border-radius: 999px; height: 10px; overflow: hidden; }
.zone-row-bar { height: 100%; border-radius: 999px; min-width: 2px; position: relative; }
.zone-row-bar-dim { position: absolute; top: 0; right: 0; bottom: 0; width: 0; background: var(--card-bg); opacity: 0.6; transition: width 0.2s; }
.zone-row-stats { color: var(--text-muted); white-space: nowrap; text-align: right; min-width: 84px; }

/* ---- Form gauge (TSB) ---- */
.tsb-gauge-wrap { margin: 0.9rem 0 1.2rem; }
.tsb-gauge { position: relative; display: flex; height: 12px; border-radius: 6px; }
.tsb-gauge-seg:first-child { border-top-left-radius: 6px; border-bottom-left-radius: 6px; }
.tsb-gauge-seg:last-child { border-top-right-radius: 6px; border-bottom-right-radius: 6px; }
.tsb-gauge-cursor { position: absolute; top: -4px; width: 12px; height: 20px; margin-left: -6px; background: var(--text); border: 2px solid var(--card-bg); border-radius: 3px; box-shadow: 0 1px 3px rgba(0,0,0,0.3); }
.tsb-status { font-weight: 600; font-size: 0.95rem; margin: 0.6rem 0 0; }
.tsb-estimate.hint { margin-top: 0.15rem; }

/* ---- Glossary tooltips ---- */
abbr[data-tip] { text-decoration: underline dotted; text-decoration-color: var(--text-muted); cursor: help; position: relative; }
abbr[data-tip]:hover::after {
  content: attr(data-tip);
  position: absolute; left: 50%; bottom: calc(100% + 6px); transform: translateX(-50%);
  width: max-content; max-width: 260px;
  background: rgba(17, 24, 39, 0.94); color: #fff;
  font-weight: 400; font-style: normal; font-size: 0.78rem; line-height: 1.35; letter-spacing: normal; text-align: left; text-transform: none;
  padding: 0.4rem 0.6rem; border-radius: 6px; box-shadow: 0 2px 8px rgba(0, 0, 0, 0.25);
  z-index: 60; pointer-events: none; white-space: normal;
}

/* ---- Calendar (FullCalendar) ---- */
#calendar {
  --fc-border-color: var(--border);
  --fc-page-bg-color: var(--card-bg);
  --fc-neutral-bg-color: var(--bg);
  --fc-today-bg-color: var(--accent-soft);
  --fc-event-text-color: #fff;
  --fc-list-event-hover-bg-color: var(--bg);
  --fc-small-font-size: 0.75rem;
  font-family: inherit;
}
#calendar .fc-daygrid-day-number { color: var(--text); font-size: 0.8rem; }
#calendar .fc-col-header-cell-cushion { color: var(--text-muted); font-size: 0.72rem; text-transform: uppercase; letter-spacing: 0.04em; font-weight: 600; }
#calendar .fc-event { border-radius: 5px; padding: 2px 4px; cursor: pointer; white-space: normal; }
#calendar .cal-ev { line-height: 1.25; font-weight: 600; text-align: center; overflow: hidden; }
#calendar .fc-daygrid-event-harness { margin-top: 2px; }
#calendar .fc-event.planned-event { border-style: dashed; border-width: 1.5px; font-weight: 500; }
#calendar .fc-daygrid-day { cursor: pointer; }
#calendar a { color: inherit; text-decoration: none; }
.calendar-hint { margin: 0 0 0.6rem; }

/* ---- Tables ---- */
.laps-table { width: 100%; border-collapse: collapse; font-size: 0.85rem; }
.laps-table th { text-align: left; color: var(--text-muted); font-weight: 600; font-size: 0.72rem; text-transform: uppercase; letter-spacing: 0.04em; padding: 0.4rem 0.5rem; border-bottom: 1px solid var(--border); }
.laps-table td { padding: 0.4rem 0.5rem; border-bottom: 1px solid var(--border); }

/* ---- Dark mode adjustments for third-party widgets ---- */
.leaflet-container { background: var(--bg); }
.map-dark .leaflet-bar a { background: var(--card-bg); color: var(--text); border-color: var(--border); }
.map-dark .leaflet-control-attribution { background: rgba(22, 27, 36, 0.8); color: var(--text-muted); }
.map-dark .leaflet-control-attribution a { color: var(--accent); }
.map-dark .leaflet-tile-pane { filter: invert(1) hue-rotate(180deg) brightness(0.95) contrast(0.9); }

/* ---- Misc ---- */
.badge { display: inline-block; font-size: 0.75rem; color: var(--text-muted); background: var(--bg); border-radius: 999px; padding: 0.1rem 0.6rem; white-space: nowrap; }
pre { white-space: pre-wrap; word-break: break-word; background: var(--bg); border-radius: var(--radius-sm); padding: 0.75rem; font-size: 0.85rem; }
@media (max-width: 640px) {
  .container { padding: 1.25rem 0.85rem 3rem; }
  section, .card { padding: 1rem; }
  h1 { font-size: 1.4rem; }
  .topbar-inner { gap: 0.5rem; padding: 0.35rem 0.85rem; }
  .nav { flex-wrap: nowrap; overflow-x: auto; width: 100%; scrollbar-width: none; }
  .nav a { padding: 0.35rem 0.6rem; white-space: nowrap; }
}
"##;
