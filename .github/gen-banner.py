#!/usr/bin/env python3
"""Generate the animated tael banner SVG.

Same recipe as the palimpsest banner: matrix glyph rain, a scrolling live
metric stream with a pulsing tracker, a typed shell command, and cycling
telemetry log lines — themed teal for `tael`.

    python3 .github/gen-banner.py > .github/tael-banner.svg
"""
import math
import random

random.seed(7701)  # tael's REST port — deterministic output

W, H = 1200, 360

# --- teal palette --------------------------------------------------------
PRIMARY = "#2dd4bf"   # tael teal
BRIGHT = "#5eead4"    # highlighted glyph / caret
GLYPH_DIM = "#115e54" # trailing rain glyphs
LOG_FILL = "#3f8a7d"
SUB_FILL = "#7fd8c9"

MONO = ("'JetBrains Mono', 'IBM Plex Mono', 'SF Mono', "
        "'DejaVu Sans Mono', Menlo, Consolas, monospace")

# glyphs: terminal punctuation + greek + otel/trace flavor
GLYPHS = list("0123456789abcdefxABCDEF<>{}[]:;=~/\\|+-.•·→↳⎯στλμπσΔΣΩ")

def esc(s):
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def rain_columns():
    out = []
    n_cols = 43
    step_x = W / n_cols
    rows = 32          # glyphs per column (covers 360 + 330 scroll)
    for c in range(n_cols):
        x = round(8 + c * step_x, 1)
        dur = round(random.uniform(3.4, 6.2), 2)
        delay = round(-random.uniform(0, dur), 2)
        bright_at = random.randrange(rows)   # which glyph leads the trail
        tspans = []
        for r in range(rows):
            y = r * 22
            g = random.choice(GLYPHS)
            if r == bright_at:
                fill, op = BRIGHT, "1.0"
            else:
                # fade ramp behind the bright head
                d = (r - bright_at) % rows
                op = round(max(0.04, 0.9 - d * 0.07), 3)
                fill = GLYPH_DIM
            tspans.append(
                f'<tspan x="{x}" y="{y}" fill="{fill}" '
                f'fill-opacity="{op}">{esc(g)}</tspan>'
            )
        out.append(
            f'    <g style="animation: rain {dur}s linear {delay}s infinite;">'
            f'<text class="glyphs">{"".join(tspans)}</text></g>'
        )
    return "\n".join(out)


# --- live metric stream --------------------------------------------------
BASE_Y, BAND = 191.0, 9.0   # graph sits in the 177..205 band
MARKER_X = 1050.0
SCROLL = 900.0

def metric_points():
    # smooth-ish random walk, enough points to scroll 900px and wrap
    pts, y = [], BASE_Y
    for i in range(94):
        y += random.uniform(-3.2, 3.2)
        y = max(BASE_Y - BAND, min(BASE_Y + BAND, y))
        pts.append((150 + i * 20, round(y, 1)))
    return pts

def y_at(pts, data_x):
    # nearest sample's y for the point currently under the marker
    best = min(pts, key=lambda p: abs(p[0] - data_x))
    return best[1]

def metric_path(pts):
    line = "M " + " L ".join(f"{x} {y}" for x, y in pts)
    last_x = pts[-1][0]
    area = line + f" L {last_x} 205 L 150 205 Z"
    return area, line

def track_keyframes(pts):
    frames = 46
    rows = []
    for k in range(frames):
        t = k / (frames - 1)
        data_x = MARKER_X + SCROLL * t
        pct = round(t * 100, 3)
        rows.append(f"        {pct}% {{ transform: translateY({y_at(pts, data_x)}px); }}")
    return "\n".join(rows)


# --- typed shell command + caret ----------------------------------------
SUBTITLE = "tael query traces --status error"
SUB_X = 400
SUB_LEN = 408           # textLength in px
CYCLE = 6.0             # seconds
TYPE_FRAC = 0.58        # fraction of cycle spent typing

def typed_clip_and_caret():
    steps = len(SUBTITLE)
    char_w = SUB_LEN / steps
    # discrete keyTimes for the reveal, then hold
    kt, widths, xs = [], [], []
    for i in range(steps + 1):
        t = (i / steps) * TYPE_FRAC
        kt.append(round(t, 5))
        widths.append(round(i * char_w, 1))
        xs.append(round(SUB_X + i * char_w, 1))
    kt.append(1.0); widths.append(widths[-1]); xs.append(xs[-1])
    kt_s = ";".join(str(v) for v in kt)
    w_s = ";".join(str(v) for v in widths)
    x_s = ";".join(str(v) for v in xs)

    clip = (
        f'    <clipPath id="subClip"><rect x="{SUB_X}" y="258" '
        f'width="{SUB_LEN}" height="28">'
        f'<animate attributeName="width" dur="{CYCLE}s" repeatCount="indefinite" '
        f'calcMode="discrete" keyTimes="{kt_s}" values="{w_s}"/></rect></clipPath>'
    )
    caret = (
        f'  <rect x="{xs[-1]}" y="265" width="11" height="19" rx="1" fill="{BRIGHT}">\n'
        f'    <animate attributeName="x" dur="{CYCLE}s" repeatCount="indefinite" '
        f'calcMode="discrete" keyTimes="{kt_s}" values="{x_s}"/>\n'
        f'    <animate attributeName="opacity" dur="{CYCLE}s" repeatCount="indefinite" '
        f'calcMode="discrete" keyTimes="0;0.58;0.64;0.7;0.76;0.82;0.88;0.94;1" '
        f'values="1;1;0;1;0;1;0;1;1"/>\n'
        f'  </rect>'
    )
    return clip, caret


LOGS = [
    "ingest OTLP :: 1,204 spans → tiered store @ block 0x3F",
    "gen_ai.completion 847 tokens · $0.0123 · claude-opus-4",
    "query traces WHERE status=error · 38ms · 12 hits",
]


def build():
    pts = metric_points()
    area, line = metric_path(pts)
    sub_clip, caret = typed_clip_and_caret()

    log_styles = []
    nlogs = len(LOGS)
    win = 100.0 / nlogs
    for i, txt in enumerate(LOGS):
        a = i * (100.0 / nlogs)
        log_styles.append(
            f"      @keyframes log{i} {{ 0%,{a:.1f}% {{opacity:0}} "
            f"{a+4:.1f}%,{a+win-6:.1f}% {{opacity:.5}} "
            f"{a+win-2:.1f}%,100% {{opacity:0}} }}"
        )
    log_kf = "\n".join(log_styles)
    log_dur = 9.6
    log_texts = "\n".join(
        f'  <text x="600" y="338" text-anchor="middle" class="logline" '
        f'style="animation: log{i} {log_dur}s ease-in-out infinite;">{txt}</text>'
        for i, txt in enumerate(LOGS)
    )

    return f'''<svg viewBox="0 0 {W} {H}" width="{W}" height="{H}" fill="none" xmlns="http://www.w3.org/2000/svg">
  <defs>
    <linearGradient id="bg" x1="0" y1="0" x2="0" y2="{H}" gradientUnits="userSpaceOnUse">
      <stop offset="0" stop-color="#02100e"/>
      <stop offset="0.55" stop-color="#04130f"/>
      <stop offset="1" stop-color="#061712"/>
    </linearGradient>
    <radialGradient id="halo" cx="600" cy="132" r="430" gradientUnits="userSpaceOnUse" gradientTransform="matrix(1 0 0 0.34 0 87)">
      <stop offset="0" stop-color="#000000" stop-opacity="0.92"/>
      <stop offset="0.6" stop-color="#000000" stop-opacity="0.72"/>
      <stop offset="1" stop-color="#000000" stop-opacity="0"/>
    </radialGradient>
    <radialGradient id="vignette" cx="600" cy="180" r="720" gradientUnits="userSpaceOnUse">
      <stop offset="0" stop-color="#000" stop-opacity="0"/>
      <stop offset="0.72" stop-color="#000" stop-opacity="0"/>
      <stop offset="1" stop-color="#000" stop-opacity="0.7"/>
    </radialGradient>
    <linearGradient id="metricArea" x1="0" y1="177" x2="0" y2="205" gradientUnits="userSpaceOnUse">
      <stop offset="0" stop-color="{PRIMARY}" stop-opacity="0.34"/>
      <stop offset="1" stop-color="{PRIMARY}" stop-opacity="0"/>
    </linearGradient>
    <linearGradient id="fadeGrad" x1="150" y1="0" x2="1050" y2="0" gradientUnits="userSpaceOnUse">
      <stop offset="0" stop-color="#fff" stop-opacity="0"/>
      <stop offset="0.5" stop-color="#fff" stop-opacity="1"/>
      <stop offset="1" stop-color="#fff" stop-opacity="1"/>
    </linearGradient>
    <mask id="metricFade"><rect x="150" y="177" width="900" height="28" fill="url(#fadeGrad)"/></mask>
    <clipPath id="metricClip"><rect x="150" y="177" width="900" height="28"/></clipPath>
    <clipPath id="rainClip"><rect x="0" y="0" width="{W}" height="{H}"/></clipPath>
{sub_clip}

    <style>
      .glyphs {{ font-family: {MONO}; font-size: 19px; font-weight: 500; }}
      .logline {{ font-family: {MONO}; font-size: 15px; fill: {LOG_FILL}; letter-spacing: 1px; opacity: 0; }}
      @keyframes rain {{ from {{ transform: translateY(-330px); }} to {{ transform: translateY(0); }} }}
      @keyframes scrollx {{ from {{ transform: translateX(0); }} to {{ transform: translateX(-{int(SCROLL)}px); }} }}
      @keyframes track {{
{track_keyframes(pts)}
      }}
      @keyframes pulse {{ 0%,100% {{ opacity: .55; transform: scale(1); }} 50% {{ opacity: 1; transform: scale(1.7); }} }}
      .metric {{ animation: scrollx 7s linear infinite; }}
      .track {{ animation: track 7s linear infinite; }}
      .pulse {{ transform-box: fill-box; transform-origin: center; animation: pulse 1.6s ease-in-out infinite; }}
{log_kf}
      @media (prefers-reduced-motion: reduce) {{
        .glyphs, .metric, .track, .pulse, .logline {{ animation: none !important; }}
        .track {{ transform: translateY({pts[0][1]}px); }}
        .logline {{ opacity: .18; }}
      }}
    </style>
  </defs>

  <rect width="{W}" height="{H}" fill="url(#bg)"/>

  <!-- matrix glyph rain: terminal punctuation x greek x trace glyphs -->
  <g clip-path="url(#rainClip)" opacity="0.6">
{rain_columns()}
  </g>

  <!-- dark halo lifts the wordmark off the rain -->
  <rect width="{W}" height="{H}" fill="url(#halo)"/>

  <!-- the wordmark -->
  <text x="600" y="132" text-anchor="middle" fill="{PRIMARY}" font-family="{MONO}" font-size="104" font-weight="700" letter-spacing="6">tael</text>

  <!-- live metric stream: scrolls left, fades out on the left half -->
  <g mask="url(#metricFade)">
    <g clip-path="url(#metricClip)">
      <g class="metric">
        <path d="{area}" fill="url(#metricArea)"/>
        <path d="{line}" fill="none" stroke="{PRIMARY}" stroke-width="2" stroke-linejoin="round" stroke-linecap="round"/>
      </g>
    </g>
  </g>
  <line x1="{int(MARKER_X)}" y1="177" x2="{int(MARKER_X)}" y2="205" stroke="{BRIGHT}" stroke-opacity="0.3" stroke-width="1"/>
  <g class="track">
    <circle cx="{int(MARKER_X)}" cy="0" r="7" fill="{BRIGHT}" opacity="0.2"/>
    <circle class="pulse" cx="{int(MARKER_X)}" cy="0" r="3.2" fill="{BRIGHT}"/>
  </g>

  <!-- typed shell command + live block caret -->
  <g clip-path="url(#subClip)">
    <text x="{SUB_X}" y="280" text-anchor="start" textLength="{SUB_LEN}" lengthAdjust="spacingAndGlyphs" fill="{SUB_FILL}" font-family="{MONO}" font-size="19">{SUBTITLE}</text>
  </g>
{caret}

  <!-- cycling telemetry log -->
{log_texts}

  <rect width="{W}" height="{H}" fill="url(#vignette)"/>
</svg>
'''


if __name__ == "__main__":
    print(build(), end="")
