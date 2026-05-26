#!/usr/bin/env python3
"""Generate the tael banner SVG — an animated trace flamegraph.

What it depicts is what tael ingests: a single distributed trace
(OTLP spans) for an AI-agent request, laid out as an icicle flamegraph
(root on top, children nested below, x-axis = wall-clock time). A reveal
sweep replays the trace left-to-right behind a playhead; the hot
`gen_ai.chat` path glows; a time ruler frames the whole 4.2s trace.

    python3 .github/gen-banner.py > .github/tael-banner.svg
"""

W, H = 1200, 360
MONO = ("'JetBrains Mono', 'IBM Plex Mono', 'SF Mono', "
        "'DejaVu Sans Mono', Menlo, Consolas, monospace")

# flamegraph geometry
GX, GW = 60, 1080          # left edge, total width
GTOP = 170                 # top of row 0
ROW = 30                   # row pitch
BAR = 25                   # bar height
GAP = 2                    # rounded-corner radius / inset
TRACE_MS = 4200            # total trace duration -> maps to GW

# palette: teal base, warm hot-path (a flamegraph that stays on-brand)
COL = {
    "http":  "#2dd4bf",    # tael teal
    "db":    "#22d3ee",    # cyan
    "cache": "#34d399",    # green
    "cpu":   "#4ade80",    # lime
    "ai":    "#fbbf24",    # amber  (LLM / hot path)
    "ai_hot":"#fb923c",    # orange (inference)
    "err":   "#f87171",    # red    (retry / error)
}
TEXT_ON_BAR = "#04201b"
CYCLE = 6.0                # seconds per replay loop


# A span = (label, duration_ms, kind, [children], hot?)
def span(label, ms, kind, children=None, hot=False):
    return {"label": label, "ms": ms, "kind": kind,
            "kids": children or [], "hot": hot}

TRACE = span("POST /v1/agent/run", TRACE_MS, "http", [
    span("auth.verify", 180, "cpu"),
    span("db.query orders", 760, "db", [
        span("pg.connect", 120, "db"),
        span("pg.exec SELECT", 540, "db"),
    ]),
    span("gen_ai.chat claude-opus-4", 2600, "ai", [
        span("build_prompt", 140, "cpu"),
        span("inference", 2200, "ai_hot", [
            span("decode tokens", 1500, "ai_hot"),
        ], hot=True),
        span("stream_response", 160, "ai"),
    ], hot=True),
    span("tools.exec", 360, "cpu", [
        span("http.fetch · retry", 300, "err"),
    ]),
    span("cache.set", 90, "cache"),
])


def layout(node, x, w, depth, out, hot_rects):
    """Recursively emit span rects; collect hot ones for the glow pass."""
    y = GTOP + depth * ROW
    fill = COL[node["kind"]]
    rx = x + GAP
    rw = max(0, w - GAP)
    rect = (f'      <rect x="{rx:.1f}" y="{y}" width="{rw:.1f}" height="{BAR}" '
            f'rx="2.5" fill="{fill}"/>')
    out.append(rect)
    if node["hot"]:
        hot_rects.append((rx, y, rw))

    # label if the bar is wide enough to hold a few chars
    if rw > 56:
        max_chars = int((rw - 14) / 7.3)
        txt = node["label"]
        if len(txt) > max_chars:
            txt = txt[:max(1, max_chars - 1)] + "…"
        out.append(
            f'      <text x="{rx + 7:.1f}" y="{y + 17}" fill="{TEXT_ON_BAR}" '
            f'font-family="{MONO}" font-size="12" font-weight="600">{esc(txt)}</text>'
        )

    # children laid out left-aligned within this span's time range
    scale = w / node["ms"]
    cx = x
    for kid in node["kids"]:
        cw = kid["ms"] * scale
        layout(kid, cx, cw, depth + 1, out, hot_rects)
        cx += cw


def esc(s):
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def time_ruler():
    """Faint vertical gridlines + second labels across the trace."""
    out = []
    px_per_ms = GW / TRACE_MS
    bottom = GTOP + 5 * ROW - (ROW - BAR)   # a touch below deepest row
    sec = 0
    while sec * 1000 <= TRACE_MS:
        x = GX + sec * 1000 * px_per_ms
        out.append(
            f'    <line x1="{x:.1f}" y1="{GTOP - 12}" x2="{x:.1f}" y2="{bottom}" '
            f'stroke="#2dd4bf" stroke-opacity="0.08" stroke-width="1"/>'
        )
        out.append(
            f'    <text x="{x + 4:.1f}" y="{GTOP - 16}" fill="#3f8a7d" '
            f'font-family="{MONO}" font-size="11">{sec}s</text>'
        )
        sec += 1
    return "\n".join(out)


def build():
    rects, hot = [], []
    layout(TRACE, GX, GW, 0, rects, hot)
    spans_svg = "\n".join(rects)

    # glow overlays for the hot (LLM) path — gentle pulse
    glow = "\n".join(
        f'    <rect class="hot" x="{x:.1f}" y="{y}" width="{w:.1f}" height="{BAR}" '
        f'rx="2.5" fill="none" stroke="#ffe7a0" stroke-width="1.5"/>'
        for x, y, w in hot
    )

    bottom = GTOP + 5 * ROW - (ROW - BAR)
    ph_top, ph_bot = GTOP - 12, bottom

    # reveal mask + playhead keyframes (replay the trace, then hold, then loop)
    # width: grow 0->GW over 0..62%, hold to 93%, snap reset under cover of fade
    mask_kt = "0;0.62;0.93;1"
    mask_w  = f"0;{GW};{GW};0"
    ph_x    = f"{GX};{GX+GW};{GX+GW};{GX}"

    return f'''<svg viewBox="0 0 {W} {H}" width="{W}" height="{H}" fill="none" xmlns="http://www.w3.org/2000/svg">
  <defs>
    <linearGradient id="bg" x1="0" y1="0" x2="0" y2="{H}" gradientUnits="userSpaceOnUse">
      <stop offset="0" stop-color="#02100e"/>
      <stop offset="0.5" stop-color="#04130f"/>
      <stop offset="1" stop-color="#061a14"/>
    </linearGradient>
    <radialGradient id="glowbg" cx="600" cy="245" r="640" gradientUnits="userSpaceOnUse" gradientTransform="matrix(1 0 0 0.42 0 142)">
      <stop offset="0" stop-color="#0c3b32" stop-opacity="0.55"/>
      <stop offset="1" stop-color="#0c3b32" stop-opacity="0"/>
    </radialGradient>
    <radialGradient id="vignette" cx="600" cy="180" r="760" gradientUnits="userSpaceOnUse">
      <stop offset="0" stop-color="#000" stop-opacity="0"/>
      <stop offset="0.7" stop-color="#000" stop-opacity="0"/>
      <stop offset="1" stop-color="#000" stop-opacity="0.6"/>
    </radialGradient>
    <linearGradient id="phGrad" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#7cf2e0" stop-opacity="0"/>
      <stop offset="0.5" stop-color="#7cf2e0" stop-opacity="0.9"/>
      <stop offset="1" stop-color="#7cf2e0" stop-opacity="0"/>
    </linearGradient>

    <clipPath id="reveal">
      <rect x="{GX}" y="{GTOP - 16}" width="0" height="200">
        <animate attributeName="width" dur="{CYCLE}s" repeatCount="indefinite"
                 keyTimes="{mask_kt}" values="{mask_w}"/>
      </rect>
    </clipPath>

    <style>
      @keyframes hotpulse {{ 0%,100% {{ opacity:.25 }} 50% {{ opacity:.9 }} }}
      .hot {{ animation: hotpulse 1.7s ease-in-out infinite; }}
      @keyframes fadeloop {{ 0% {{opacity:0}} 5% {{opacity:1}} 90% {{opacity:1}} 99%,100% {{opacity:0}} }}
      .replay {{ animation: fadeloop {CYCLE}s linear infinite; }}
      @media (prefers-reduced-motion: reduce) {{
        .hot {{ animation: none !important; opacity:.6 }}
        .replay {{ animation: none !important; opacity:1 }}
      }}
    </style>
  </defs>

  <rect width="{W}" height="{H}" fill="url(#bg)"/>
  <rect width="{W}" height="{H}" fill="url(#glowbg)"/>

  <!-- wordmark + tagline -->
  <text x="{GX}" y="96" fill="#2dd4bf" font-family="{MONO}" font-size="78" font-weight="700" letter-spacing="3">tael</text>
  <text x="{GX + 4}" y="126" fill="#5ec8b8" font-family="{MONO}" font-size="16" letter-spacing="0.5">AI-agent-native observability · OTLP traces · logs · metrics</text>

  <!-- trace caption (top-right) -->
  <text x="{GX + GW}" y="96" text-anchor="end" fill="#3f8a7d" font-family="{MONO}" font-size="13">trace 7f3a…c1 · {TRACE_MS/1000:.1f}s · 12 spans</text>
  <text x="{GX + GW}" y="126" text-anchor="end" fill="#fbbf24" font-family="{MONO}" font-size="13" opacity="0.85">▮ gen_ai.chat · 847 tok · $0.0123</text>

  <!-- time ruler -->
{time_ruler()}

  <!-- the flamegraph: drawn once, revealed by the sweep, looping -->
  <g class="replay">
    <g clip-path="url(#reveal)">
{spans_svg}
{glow}
    </g>

    <!-- playhead riding the reveal edge -->
    <rect x="{GX}" y="{ph_top}" width="2" height="{ph_bot - ph_top}" fill="url(#phGrad)">
      <animate attributeName="x" dur="{CYCLE}s" repeatCount="indefinite"
               keyTimes="{mask_kt}" values="{ph_x}"/>
    </rect>
    <circle cx="{GX}" cy="{ph_top}" r="3.5" fill="#7cf2e0">
      <animate attributeName="cx" dur="{CYCLE}s" repeatCount="indefinite"
               keyTimes="{mask_kt}" values="{ph_x}"/>
    </circle>
  </g>

  <rect width="{W}" height="{H}" fill="url(#vignette)"/>
</svg>
'''


if __name__ == "__main__":
    print(build(), end="")
