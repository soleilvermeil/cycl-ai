//! TrainingPeaks-style "step" chart of a structured workout: one block per
//! step, width = duration, height = target intensity, colored by zone.
//!
//! One JS implementation serves every place a workout is drawn (the live
//! preview in the builder, the /workouts list, the dashboard digest), so the
//! picture can never differ between them. Pages embed `CHART_JS` once in
//! `<head>`; any `<div class="workout-chart" data-workout="…" data-ftp="…">`
//! is drawn on load, and the builder calls `window.renderWorkoutChart`
//! directly whenever the workout changes.
//!
//! Power targets are drawn at their watts. Heart-rate targets (range or zone)
//! have no watts, so they're drawn at the power typical of their HR zone
//! (`workout::HR_ZONE_POWER_FRAC`, mirrored below) with a dashed outline and
//! their own label — the height is an equivalence, the label says what the
//! target really is.

use super::escape_html;
use crate::workout::Athlete;

pub(super) const CHART_CSS: &str = r#"
.workout-chart svg { display: block; width: 100%; height: auto; }
.workout-chart .wc-axis { fill: var(--text-muted); font-size: 10px; }
.workout-chart .wc-grid { stroke: var(--border); stroke-width: 1; }
.workout-chart .wc-ftp { stroke: var(--text); stroke-width: 1; stroke-dasharray: 4 3; opacity: 0.55; }
.workout-chart .wc-label { font-size: 9px; font-weight: 600; pointer-events: none; }
.workout-chart-empty { color: var(--text-muted); font-size: 0.85rem; padding: 1.2rem 0; text-align: center; }
"#;

/// `<script>` defining `window.renderWorkoutChart(el, workout, ftp, hr)`,
/// `window.workoutStats(workout, ftp, hr)`, `window.parseWorkoutMinutes(str)`
/// and the HR helpers the builder uses. `hr` is `{rest, max}` (bpm).
/// Duration syntax mirrors `workout::parse_minutes` (15min, 90s, 1h, 1h30, 1:30).
pub(super) const CHART_JS: &str = r####"<script>
(function() {
  // Same palette and %FTP bounds as the dashboard's zone bar (web/mod.rs:
  // ZONE_COLORS / ZONE_BOUNDS) — keep in sync.
  var ZONE_COLORS = ['#9ca3af', '#3b82f6', '#22c55e', '#eab308', '#f97316', '#dc2626', '#a855f7'];
  var ZONE_UPPER = [0.55, 0.75, 0.90, 1.05, 1.20, 1.50];
  // Karvonen HR zones (fraction of reserve) and their typical power as a
  // fraction of FTP — mirrors workout::HR_ZONE_UPPER / HR_ZONE_POWER_FRAC.
  var HR_UPPER = [0.60, 0.70, 0.80, 0.90];
  var HR_FRAC = [0.50, 0.65, 0.82, 0.97, 1.10];
  var HR_ZONE_NAMES = ['Récupération', 'Endurance', 'Tempo', 'Seuil', 'VO2max'];
  // The 7 power zones as (low, high) fractions of FTP, mirroring
  // workout::POWER_ZONE_RANGES_FRAC (open ends capped at 40 % / 180 %).
  var POWER_ZONE_FRAC = [[0.40, 0.55], [0.55, 0.75], [0.75, 0.90], [0.90, 1.05], [1.05, 1.20], [1.20, 1.50], [1.50, 1.80]];
  var POWER_ZONE_NAMES = ['Récupération active', 'Endurance', 'Tempo', 'Seuil', 'VO2max', 'Capacité anaérobie', 'Neuromusculaire'];
  var DEFAULT_HR = {rest: 50, max: 190};
  var TYPE_LABELS = {warmup: 'Échauffement', interval: 'Intervalle', recovery: 'Récupération', cooldown: 'Retour au calme'};

  function num(x) { var v = parseFloat(x); return isFinite(v) ? v : 0; }
  function zoneIndex(frac) {
    for (var i = 0; i < ZONE_UPPER.length; i++) if (frac < ZONE_UPPER[i]) return i;
    return 6;
  }
  function zoneColor(frac) { return ZONE_COLORS[zoneIndex(frac)]; }

  function hrOf(hr) { return hr && hr.max > hr.rest ? hr : DEFAULT_HR; }
  function hrZoneOf(bpm, hr) {
    hr = hrOf(hr);
    var frac = (bpm - hr.rest) / (hr.max - hr.rest);
    for (var i = 0; i < HR_UPPER.length; i++) if (frac < HR_UPPER[i]) return i + 1;
    return 5;
  }
  // [low, high] bpm of HR zone z (1-5); zone 5 tops out at max HR.
  function hrZoneBpm(z, hr) {
    hr = hrOf(hr);
    var hrr = hr.max - hr.rest;
    var lo = z === 1 ? 0 : HR_UPPER[z - 2];
    var hi = z === 5 ? 1 : HR_UPPER[z - 1];
    return [Math.round(hr.rest + lo * hrr), Math.round(hr.rest + hi * hrr)];
  }

  function fmtPace(secs) {
    var total = Math.round(secs);
    return Math.floor(total / 60) + ':' + String(total % 60).padStart(2, '0');
  }
  function powerZoneRange(z, ftp) {
    var r = POWER_ZONE_FRAC[Math.min(7, Math.max(1, z)) - 1];
    return [Math.round(r[0] * ftp), Math.round(r[1] * ftp)];
  }

  // A step's target as an intensity on the watts axis: {kind, lo, hi, label}.
  // kind 'power' is exact (watts or a power zone's band); 'hr' (heart rate)
  // and 'pace' (running pace, relative to hr.runPace = threshold pace in s/km)
  // are placed through an intensity equivalence.
  function leafIntensity(target, ftp, hr) {
    var t = target && target.type;
    if (t === 'power_zone') {
      var pz = Math.min(7, Math.max(1, parseInt(target.zone, 10) || 1));
      var r = POWER_ZONE_FRAC[pz - 1], w = powerZoneRange(pz, ftp);
      return {kind: 'power', lo: r[0] * ftp, hi: r[1] * ftp, label: 'Z' + pz,
              detail: 'Zone ' + pz + ' ' + POWER_ZONE_NAMES[pz - 1] + ' (' + w[0] + '–' + w[1] + ' W)'};
    }
    if (t === 'pace_range') {
      var fast = num(target.low), slow = num(target.high);
      if (slow < fast) slow = fast;
      var thr = hr && hr.runPace > 0 ? hr.runPace : 0;
      function frac(p) { return thr > 0 && p > 0 ? Math.min(1.3, Math.max(0.4, thr / p)) : 0.75; }
      var txt = fmtPace(fast) + (slow !== fast ? '–' + fmtPace(slow) : '') + ' /km';
      return {kind: 'pace', lo: frac(slow) * ftp, hi: frac(fast) * ftp, label: txt, detail: txt};
    }
    if (t === 'hr_zone') {
      var z = Math.min(5, Math.max(1, parseInt(target.zone, 10) || 1));
      var w = HR_FRAC[z - 1] * ftp, b = hrZoneBpm(z, hr);
      return {kind: 'hr', lo: w, hi: w, label: 'Z' + z + ' FC', detail: 'Zone FC ' + z + ' (' + b[0] + '–' + b[1] + ' bpm)'};
    }
    if (t === 'hr_range') {
      var l = num(target.low), h = num(target.high);
      if (h < l) h = l;
      var zl = hrZoneOf(l, hr), zh = hrZoneOf(h, hr);
      var txt = Math.round(l) + (h !== l ? '–' + Math.round(h) : '');
      return {kind: 'hr', lo: HR_FRAC[zl - 1] * ftp, hi: HR_FRAC[zh - 1] * ftp, label: txt + ' bpm', detail: txt + ' bpm'};
    }
    var lo = num(target && target.low), hi = num(target && target.high);
    if (hi < lo) hi = lo;
    return {kind: 'power', lo: lo, hi: hi, label: null, detail: null};
  }

  function parseMinutes(raw) {
    var v = String(raw == null ? '' : raw).trim().toLowerCase().replace(',', '.');
    var m;
    if ((m = v.match(/^([\d.]+)\s*h\s*(?:([\d.]+)\s*(?:min)?)?$/))) return num(m[1]) * 60 + (m[2] ? num(m[2]) : 0);
    if ((m = v.match(/^([\d.]+)\s*min$/))) return num(m[1]);
    if ((m = v.match(/^([\d.]+)\s*(?:sec|s)$/))) return num(m[1]) / 60;
    if ((m = v.match(/^(\d+):(\d{1,2})$/))) return parseInt(m[1], 10) + parseInt(m[2], 10) / 60;
    if (/^[\d.]+$/.test(v)) return num(v);
    return 0;
  }

  // Expands repeats into a flat, time-ordered list of blocks.
  function flatten(steps, out, t, ftp, hr) {
    (steps || []).forEach(function(s) {
      if (s.type === 'repeat') {
        var n = Math.max(1, parseInt(s.count, 10) || 1);
        for (var k = 0; k < n; k++) t = flatten(s.steps, out, t, ftp, hr);
        return;
      }
      var d = parseMinutes(s.duration && s.duration.value);
      if (d <= 0) return;
      var it = leafIntensity(s.target, ftp, hr);
      out.push({type: s.type, start: t, dur: d, low: it.lo, high: it.hi, kind: it.kind, label: it.label, detail: it.detail});
      t += d;
    });
    return t;
  }

  function fmtDur(min) {
    var total = Math.round(min * 60);
    var h = Math.floor(total / 3600), m = Math.floor((total % 3600) / 60), s = total % 60;
    return h + ':' + String(m).padStart(2, '0') + (s ? ':' + String(s).padStart(2, '0') : '');
  }
  function fmtTick(min) {
    if (min >= 60) return Math.floor(min / 60) + 'h' + (min % 60 ? String(Math.round(min % 60)).padStart(2, '0') : '');
    return min + "'";
  }

  // Duration + rough load of a workout. TSS/IF are estimates from each step's
  // average target (no variability term; HR steps use the zone equivalence),
  // good enough to compare sessions.
  function workoutStats(workout, ftp, hr) {
    var blocks = [];
    var total = flatten(workout && workout.steps, blocks, 0, ftp, hr);
    var hasInvalidRange = false;
    var tss = 0;
    blocks.forEach(function(b) {
      var f = (b.low + b.high) / 2 / (ftp || 1);
      tss += b.dur / 60 * f * f * 100;
    });
    (function scan(steps) {
      (steps || []).forEach(function(s) {
        if (s.type === 'repeat') return scan(s.steps);
        var t = s.target;
        if (t && (t.type === 'power_range' || t.type === 'hr_range' || t.type === 'pace_range') && num(t.low) > num(t.high)) hasInvalidRange = true;
      });
    })(workout && workout.steps);
    return {
      minutes: total,
      tss: tss,
      intensity: total > 0 ? Math.sqrt(tss / (total / 60) / 100) : 0,
      blocks: blocks,
      hasInvalidRange: hasInvalidRange
    };
  }

  function esc(s) { return String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;'); }

  function renderWorkoutChart(el, workout, ftp, hr) {
    ftp = ftp > 0 ? ftp : 200;
    var st = workoutStats(workout, ftp, hr);
    if (!st.blocks.length) {
      el.innerHTML = '<div class="workout-chart-empty">Ajoute des étapes pour voir la séance.</div>';
      return st;
    }
    var W = 720, H = 220, L = 44, R = 12, T = 14, B = 26;
    var pw = W - L - R, ph = H - T - B;
    var maxHigh = Math.max.apply(null, st.blocks.map(function(b) { return b.high; }));
    var yTop = Math.ceil(Math.max(maxHigh, ftp * 1.2) * 1.05 / 50) * 50;
    var gridStep = yTop > 450 ? 100 : 50;
    function x(min) { return L + min / st.minutes * pw; }
    function y(w) { return T + ph - w / yTop * ph; }

    var svg = '<svg viewBox="0 0 ' + W + ' ' + H + '" role="img" aria-label="Profil de la séance">';
    for (var g = 0; g <= yTop; g += gridStep) {
      svg += '<line class="wc-grid" x1="' + L + '" x2="' + (W - R) + '" y1="' + y(g) + '" y2="' + y(g) + '"/>' +
             '<text class="wc-axis" x="' + (L - 6) + '" y="' + (y(g) + 3) + '" text-anchor="end">' + g + '</text>';
    }
    var tickSteps = [1, 2, 5, 10, 15, 20, 30, 60, 120];
    var tickStep = tickSteps.filter(function(c) { return st.minutes / c <= 10; })[0] || 240;
    for (var t = 0; t <= st.minutes + 1e-6; t += tickStep) {
      svg += '<text class="wc-axis" x="' + x(t) + '" y="' + (H - 8) + '" text-anchor="middle">' + fmtTick(t) + '</text>';
    }
    st.blocks.forEach(function(b) {
      var avg = (b.low + b.high) / 2;
      var color = zoneColor(avg / ftp);
      // Heart-rate and pace targets are drawn at an equivalent intensity (dashed).
      var isHr = b.kind !== 'power';
      var x0 = x(b.start), bw = Math.max(1, x(b.start + b.dur) - x0);
      var tip = (TYPE_LABELS[b.type] || b.type) + ' · ' + fmtDur(b.dur) + ' · ' +
        (isHr
          ? b.detail + ' (≈ ' + Math.round(avg) + ' W équiv.)'
          : b.detail
          ? b.detail
          : (b.low === b.high ? Math.round(b.low) : Math.round(b.low) + '–' + Math.round(b.high)) + ' W (' + Math.round(avg / ftp * 100) + ' % FTP)');
      // Solid up to the low target, translucent up to the high one; heart-rate
      // targets get a dashed outline.
      svg += '<g><title>' + esc(tip) + '</title>' +
        '<rect x="' + x0 + '" y="' + y(b.high) + '" width="' + bw + '" height="' + (y(0) - y(b.high)) +
        '" fill="' + color + '" fill-opacity="0.35"/>' +
        '<rect x="' + x0 + '" y="' + y(b.low) + '" width="' + bw + '" height="' + (y(0) - y(b.low)) +
        '" fill="' + color + '" style="stroke:' + (isHr ? 'var(--text)' : 'var(--card-bg)') + '" stroke-width="1"' +
        (isHr ? ' stroke-dasharray="3 2"' : '') + '/>';
      var label = b.label ? b.label : String(Math.round(avg));
      if (bw >= (b.label && b.label.length > 4 ? 46 : 30) && y(0) - y(b.low) >= 16) {
        svg += '<text class="wc-label" x="' + (x0 + bw / 2) + '" y="' + (y(b.low) + 12) +
          '" text-anchor="middle" fill="' + (color === '#eab308' ? '#1f2430' : '#fff') + '">' + esc(label) + '</text>';
      }
      svg += '</g>';
    });
    svg += '<line class="wc-ftp" x1="' + L + '" x2="' + (W - R) + '" y1="' + y(ftp) + '" y2="' + y(ftp) + '"/>' +
           '<text class="wc-axis" x="' + (W - R) + '" y="' + (y(ftp) - 3) + '" text-anchor="end">FTP ' + Math.round(ftp) + ' W</text>';
    svg += '</svg>';
    el.innerHTML = svg;
    return st;
  }

  window.parseWorkoutMinutes = parseMinutes;
  window.workoutStats = workoutStats;
  window.renderWorkoutChart = renderWorkoutChart;
  window.workoutZoneColor = zoneColor;
  window.workoutFmtDur = fmtDur;
  window.workoutLeafIntensity = leafIntensity;
  window.workoutHrZoneOf = hrZoneOf;
  window.workoutHrZoneBpm = hrZoneBpm;
  window.workoutHrZoneNames = HR_ZONE_NAMES;
  window.workoutPowerZoneRange = powerZoneRange;
  window.workoutPowerZoneNames = POWER_ZONE_NAMES;
  window.workoutPowerZoneOf = function(frac) { return zoneIndex(frac) + 1; };
  window.workoutFmtPace = fmtPace;
  window.workoutParsePace = function(raw) {
    var v = String(raw == null ? '' : raw).trim().replace(',', '.'), m;
    if ((m = v.match(/^(\d+):(\d{1,2})$/))) return parseInt(m[1], 10) * 60 + parseInt(m[2], 10);
    return /^[\d.]+$/.test(v) ? num(v) : 0;
  };

  document.addEventListener('DOMContentLoaded', function() {
    document.querySelectorAll('.workout-chart[data-workout]').forEach(function(el) {
      try {
        var hr = {rest: parseFloat(el.getAttribute('data-hr-rest')), max: parseFloat(el.getAttribute('data-hr-max')),
                  runPace: parseFloat(el.getAttribute('data-run-pace')) || 0};
        renderWorkoutChart(el, JSON.parse(el.getAttribute('data-workout')), parseFloat(el.getAttribute('data-ftp')), hr);
      } catch (e) { el.textContent = ''; }
    });
  });
})();
</script>"####;

/// Placeholder `<div>` that `CHART_JS` fills in on page load.
pub(super) fn chart_div(workout_json: &str, athlete: &Athlete) -> String {
    // escape_html leaves quotes alone (it's for text nodes), but this goes
    // into a double-quoted attribute.
    format!(
        r#"<div class="workout-chart" data-ftp="{}" data-hr-rest="{}" data-hr-max="{}" data-run-pace="{}" data-workout="{}"></div>"#,
        athlete.ftp_watts.round(),
        athlete.hr_rest.round(),
        athlete.hr_max.round(),
        athlete.run_threshold_pace_s.round(),
        escape_html(workout_json).replace('"', "&quot;")
    )
}
