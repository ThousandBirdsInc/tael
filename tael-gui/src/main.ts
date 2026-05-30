import {invoke} from '@tauri-apps/api/core'
import {listen, UnlistenFn} from '@tauri-apps/api/event'
import './styles.css'

type Json = null | boolean | number | string | Json[] | {[key: string]: Json}

type SpanRow = {
  traceId: string
  spanId: string
  parentSpanId: string | null
  service: string
  operation: string
  durationMs: number
  status: string
  startTime: string
  startTimeMs: number
  attributes: Record<string, Json>
  events: Json[]
}

type ServiceRow = {
  name: string
  spanCount: number
  traceCount: number
  avgDurationMs: number
  errorRate: number
}

type LiveTraceRow = {
  traceId: string
  service: string
  operation: string
  startTimeMs: number
  endTimeMs: number
  durationMs: number
  spanCount: number
  hasError: boolean
}

type WaterfallRow = {
  spanIdx: number
  depth: number
  offsetPct: number
  widthPct: number
}

type CommentRow = {
  author: string
  body: string
  createdAt: string
  spanId: string | null
}

type EvalRunRow = {
  runId: string
  suiteId: string
  status: string
  caseCount: number | null
  observedCases: number
  scoredCases: number
  passedCases: number
  failedCases: number
  costUsd: number
  avgScores: Record<string, Json>
}

type EvalCaseRow = {
  caseId: string
  status: string
  traceId: string | null
  durationMs: number | null
  costUsd: number
  scores: Record<string, Json>
  comments: CommentRow[]
}

type LivePayload = {
  streamId: string
  data: string
}

type LiveStatusPayload = {
  streamId: string
  status: string
  message: string | null
}

type Tab = 'traces' | 'services' | 'evals' | 'timeline' | 'detail'

type AppState = {
  server: string
  serviceFilter: string
  statusFilter: string
  lastWindow: string
  textFilter: string
  pinnedColumns: string[]
  attrPickerOpen: boolean
  spanViewer: SpanRow | null
  tab: Tab
  prevTab: Tab
  paused: boolean
  connection: string
  error: string | null
  streamId: string
  spans: SpanRow[]
  selectedSpanIdx: number | null
  services: ServiceRow[]
  selectedServiceIdx: number | null
  liveTraceMap: Map<string, LiveTraceRow>
  liveTraces: LiveTraceRow[]
  selectedTraceIdx: number | null
  timelineWindowMs: number
  traceSpans: SpanRow[]
  waterfallRows: WaterfallRow[]
  selectedWaterfallIdx: number | null
  currentTraceId: string | null
  comments: CommentRow[]
  commentDraft: string
  evalRun: EvalRunRow | null
  evalCases: EvalCaseRow[]
  selectedEvalIdx: number | null
  evalFailuresOnly: boolean
  detailZoom: {start: number; end: number}
  liveZoom: {start: number; end: number}
}

const MAX_LIVE_SPANS = 200
const MAX_LIVE_TRACES = 500
const CANVAS_FONT = '12px "BerkeleyMono", ui-monospace, Menlo, Consolas, monospace'
const CANVAS_AXIS_FONT = '11px "BerkeleyMono", ui-monospace, Menlo, Consolas, monospace'
const CANVAS_BG = '#141414'
const CANVAS_ROW_ALT = '#181818'
const CANVAS_ROW_SELECTED = '#2b2611'
const CANVAS_AXIS = '#2a2a2a'
const CANVAS_TEXT = '#b5b5b1'
const CANVAS_FAINT = '#6f6f6c'
const CANVAS_ERROR = '#ef4444'

function streamId(): string {
  if (typeof crypto !== 'undefined' && 'randomUUID' in crypto) {
    return crypto.randomUUID()
  }
  return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`
}

const state: AppState = {
  server: 'http://127.0.0.1:7701',
  serviceFilter: '',
  statusFilter: '',
  lastWindow: '1h',
  textFilter: '',
  pinnedColumns: [],
  attrPickerOpen: false,
  spanViewer: null,
  tab: 'traces',
  prevTab: 'traces',
  paused: false,
  connection: 'idle',
  error: null,
  streamId: streamId(),
  spans: [],
  selectedSpanIdx: null,
  services: [],
  selectedServiceIdx: null,
  liveTraceMap: new Map(),
  liveTraces: [],
  selectedTraceIdx: null,
  timelineWindowMs: 60_000,
  traceSpans: [],
  waterfallRows: [],
  selectedWaterfallIdx: null,
  currentTraceId: null,
  comments: [],
  commentDraft: '',
  evalRun: null,
  evalCases: [],
  selectedEvalIdx: null,
  evalFailuresOnly: false,
  detailZoom: {start: 0, end: 1},
  liveZoom: {start: 0, end: 1},
}

let liveUnlisten: UnlistenFn | null = null
let liveStatusUnlisten: UnlistenFn | null = null
let renderQueued = false
let refreshTimer: number | null = null

const appRoot = document.querySelector<HTMLDivElement>('#app')
if (!appRoot) throw new Error('missing #app')
const app: HTMLDivElement = appRoot

function queueRender() {
  if (renderQueued) return
  renderQueued = true
  requestAnimationFrame(() => {
    renderQueued = false
    render()
  })
}

function escapeHtml(value: unknown): string {
  return String(value ?? '')
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
}

function parseTimeMs(value: string): number {
  const parsed = Date.parse(value)
  return Number.isFinite(parsed) ? parsed : 0
}

function parseSpans(value: any): SpanRow[] {
  const raw = Array.isArray(value) ? value : Array.isArray(value?.spans) ? value.spans : []
  return raw.map((span: any) => {
    const startTime = String(span.start_time ?? span.startTime ?? '-')
    return {
      traceId: String(span.trace_id ?? span.traceId ?? '-'),
      spanId: String(span.span_id ?? span.spanId ?? '-'),
      parentSpanId: span.parent_span_id ?? span.parentSpanId ?? null,
      service: String(span.service ?? '-'),
      operation: String(span.operation ?? '-'),
      durationMs: Number(span.duration_ms ?? span.durationMs ?? 0),
      status: String(span.status ?? '-'),
      startTime,
      startTimeMs: parseTimeMs(startTime),
      attributes: (span.attributes && typeof span.attributes === 'object'
        ? span.attributes
        : {}) as Record<string, Json>,
      events: Array.isArray(span.events) ? span.events : [],
    }
  })
}

function parseServices(value: any): ServiceRow[] {
  return (Array.isArray(value?.services) ? value.services : []).map((service: any) => ({
    name: String(service.name ?? '-'),
    spanCount: Number(service.span_count ?? service.spanCount ?? 0),
    traceCount: Number(service.trace_count ?? service.traceCount ?? 0),
    avgDurationMs: Number(service.avg_duration_ms ?? service.avgDurationMs ?? 0),
    errorRate: Number(service.error_rate ?? service.errorRate ?? 0),
  }))
}

function parseComments(value: any): CommentRow[] {
  return (Array.isArray(value?.comments) ? value.comments : []).map((comment: any) => ({
    author: String(comment.author ?? '-'),
    body: String(comment.body ?? ''),
    createdAt: String(comment.created_at ?? comment.createdAt ?? '-'),
    spanId: comment.span_id ?? comment.spanId ?? null,
  }))
}

function parseEvalRun(value: any): EvalRunRow | null {
  if (!value) return null
  return {
    runId: String(value.run_id ?? value.runId ?? '-'),
    suiteId: String(value.suite_id ?? value.suiteId ?? '-'),
    status: String(value.status ?? '-'),
    caseCount: value.case_count ?? value.caseCount ?? null,
    observedCases: Number(value.observed_cases ?? value.observedCases ?? 0),
    scoredCases: Number(value.scored_cases ?? value.scoredCases ?? 0),
    passedCases: Number(value.passed_cases ?? value.passedCases ?? 0),
    failedCases: Number(value.failed_cases ?? value.failedCases ?? 0),
    costUsd: Number(value.cost_usd ?? value.costUsd ?? 0),
    avgScores: (value.avg_scores ?? value.avgScores ?? {}) as Record<string, Json>,
  }
}

function parseEvalCases(value: any): EvalCaseRow[] {
  return (Array.isArray(value?.cases) ? value.cases : []).map((item: any) => ({
    caseId: String(item.case_id ?? item.caseId ?? '-'),
    status: String(item.status ?? '-'),
    traceId: item.trace_id ?? item.traceId ?? null,
    durationMs: item.duration_ms ?? item.durationMs ?? null,
    costUsd: Number(item.cost_usd ?? item.costUsd ?? 0),
    scores: (item.scores ?? {}) as Record<string, Json>,
    comments: parseComments({comments: item.comments}),
  }))
}

function serviceColor(service: string): string {
  const palette = [
    '#facc15',
    '#62a9ff',
    '#52d284',
    '#b78cff',
    '#f59e8c',
    '#5ad1c9',
    '#e0a3ff',
    '#8fc4ff',
    '#d4b483',
    '#ff9ab0',
  ]
  let hash = 0
  for (const char of service) hash = (hash * 31 + char.charCodeAt(0)) >>> 0
  return palette[hash % palette.length]
}

function durationClass(ms: number): string {
  if (ms >= 500) return 'danger'
  if (ms >= 100) return 'warn'
  return 'ok'
}

function statusClass(status: string): string {
  if (status === 'error' || status === 'fail') return 'danger'
  if (status === 'ok' || status === 'pass') return 'ok'
  return 'muted'
}

function shortTime(value: string): string {
  const time = value.includes('T') ? value.split('T')[1] : value
  return time.replace(/Z$/, '').slice(0, 12)
}

function shortId(value: string, len = 16): string {
  return value.length > len ? `${value.slice(0, len)}...` : value
}

function attrValue(span: SpanRow, key: string): string {
  const value = span.attributes[key]
  if (value == null) return ''
  return typeof value === 'string' ? value : JSON.stringify(value)
}

function attributeKeys(): string[] {
  const selected = selectedSpan() ?? selectedWaterfallSpan()
  const seen = new Set<string>()
  const keys: string[] = []

  const collect = (span: SpanRow | null) => {
    if (!span) return
    for (const key of Object.keys(span.attributes)) {
      if (!seen.has(key)) {
        seen.add(key)
        keys.push(key)
      }
    }
  }

  collect(selected)
  for (const span of state.spans) collect(span)
  for (const span of state.traceSpans) collect(span)
  return keys
}

function togglePinnedColumn(key: string) {
  const idx = state.pinnedColumns.indexOf(key)
  if (idx >= 0) {
    state.pinnedColumns.splice(idx, 1)
  } else {
    state.pinnedColumns.push(key)
  }
}

function filteredSpans(): SpanRow[] {
  const q = state.textFilter.trim().toLowerCase()
  if (!q) return state.spans
  return state.spans.filter(
    span =>
      span.service.toLowerCase().includes(q) ||
      span.operation.toLowerCase().includes(q) ||
      span.traceId.toLowerCase().includes(q) ||
      span.status.toLowerCase().includes(q),
  )
}

function filteredLiveTraces(): LiveTraceRow[] {
  const q = state.textFilter.trim().toLowerCase()
  if (!q) return state.liveTraces
  return state.liveTraces.filter(
    trace =>
      trace.service.toLowerCase().includes(q) ||
      trace.operation.toLowerCase().includes(q) ||
      trace.traceId.toLowerCase().includes(q) ||
      (trace.hasError ? 'error' : 'ok').includes(q),
  )
}

function filteredEvalCases(): EvalCaseRow[] {
  const q = state.textFilter.trim().toLowerCase()
  return state.evalCases.filter(item => {
    if (state.evalFailuresOnly && item.status !== 'fail') return false
    if (!q) return true
    return (
      item.caseId.toLowerCase().includes(q) ||
      item.status.toLowerCase().includes(q) ||
      (item.traceId ?? '').toLowerCase().includes(q)
    )
  })
}

function buildWaterfall(spans: SpanRow[]): WaterfallRow[] {
  if (spans.length === 0) return []
  const traceStart = Math.min(...spans.map(span => span.startTimeMs))
  const traceEnd = Math.max(...spans.map(span => span.startTimeMs + span.durationMs))
  const traceDuration = Math.max(traceEnd - traceStart, 1)
  const children = new Map<string, number[]>()
  const rootKey = '__root__'

  spans.forEach((span, idx) => {
    const key = span.parentSpanId ?? rootKey
    const bucket = children.get(key) ?? []
    bucket.push(idx)
    children.set(key, bucket)
  })

  const rows: WaterfallRow[] = []
  const stack: Array<{parent: string; depth: number}> = [{parent: rootKey, depth: 0}]
  while (stack.length > 0) {
    const frame = stack.pop()!
    const childIndices = children.get(frame.parent) ?? []
    for (const idx of [...childIndices].reverse()) {
      const span = spans[idx]
      rows.push({
        spanIdx: idx,
        depth: frame.depth,
        offsetPct: clamp((span.startTimeMs - traceStart) / traceDuration, 0, 1),
        widthPct: clamp(span.durationMs / traceDuration, 0.005, 1),
      })
      stack.push({parent: span.spanId, depth: frame.depth + 1})
    }
  }

  const visited = new Set(rows.map(row => row.spanIdx))
  spans.forEach((span, idx) => {
    if (visited.has(idx)) return
    rows.push({
      spanIdx: idx,
      depth: 0,
      offsetPct: clamp((span.startTimeMs - traceStart) / traceDuration, 0, 1),
      widthPct: clamp(span.durationMs / traceDuration, 0.005, 1),
    })
  })
  return rows
}

function clamp(value: number, min: number, max: number): number {
  return Math.max(min, Math.min(max, value))
}

function updateLiveTraces(spans: SpanRow[]) {
  for (const span of spans) {
    const endTimeMs = span.startTimeMs + span.durationMs
    const existing = state.liveTraceMap.get(span.traceId)
    if (!existing) {
      state.liveTraceMap.set(span.traceId, {
        traceId: span.traceId,
        service: span.service,
        operation: span.operation,
        startTimeMs: span.startTimeMs,
        endTimeMs,
        durationMs: span.durationMs,
        spanCount: 1,
        hasError: span.status === 'error',
      })
      continue
    }
    existing.startTimeMs = Math.min(existing.startTimeMs, span.startTimeMs)
    existing.endTimeMs = Math.max(existing.endTimeMs, endTimeMs)
    existing.durationMs = existing.endTimeMs - existing.startTimeMs
    existing.spanCount += 1
    existing.hasError ||= span.status === 'error'
    if (!span.parentSpanId) {
      existing.service = span.service
      existing.operation = span.operation
    }
  }

  state.liveTraces = [...state.liveTraceMap.values()].sort((a, b) => a.startTimeMs - b.startTimeMs)
  if (state.liveTraces.length > MAX_LIVE_TRACES) {
    const remove = state.liveTraces.slice(0, state.liveTraces.length - MAX_LIVE_TRACES)
    for (const trace of remove) state.liveTraceMap.delete(trace.traceId)
    state.liveTraces = state.liveTraces.slice(-MAX_LIVE_TRACES)
  }
}

async function refreshTraces() {
  const value = await invoke<Json>('query_traces', {
    server: state.server,
    request: {
      service: state.serviceFilter || null,
      status: state.statusFilter || null,
      last: state.lastWindow || '1h',
      limit: 200,
      text: state.textFilter || null,
    },
  })
  state.spans = parseSpans(value)
  updateLiveTraces(state.spans)
}

async function refreshServices() {
  state.services = parseServices(await invoke<Json>('list_services', {server: state.server}))
}

async function refreshEvals() {
  const runsValue: any = await invoke<Json>('eval_runs', {server: state.server})
  const firstRun = Array.isArray(runsValue?.runs) ? runsValue.runs[0] : null
  const runId = firstRun?.run_id ?? firstRun?.runId
  if (!runId) {
    state.evalRun = null
    state.evalCases = []
    return
  }

  const statusValue: any = await invoke<Json>('eval_status', {server: state.server, runId})
  state.evalRun = parseEvalRun(statusValue?.run ?? statusValue)
  state.evalCases = parseEvalCases(await invoke<Json>('eval_cases', {server: state.server, runId}))
}

async function loadTrace(traceId: string) {
  state.prevTab = state.tab === 'detail' ? state.prevTab : state.tab
  state.tab = 'detail'
  state.currentTraceId = traceId
  state.selectedWaterfallIdx = null
  state.traceSpans = []
  state.waterfallRows = []
  state.comments = []
  state.detailZoom = {start: 0, end: 1}
  state.error = null
  queueRender()

  try {
    const [traceValue, commentsValue] = await Promise.all([
      invoke<Json>('get_trace', {server: state.server, traceId}),
      invoke<Json>('get_comments', {server: state.server, traceId}),
    ])
    state.traceSpans = parseSpans(traceValue)
    state.waterfallRows = buildWaterfall(state.traceSpans)
    state.selectedWaterfallIdx = state.waterfallRows.length > 0 ? 0 : null
    state.comments = parseComments(commentsValue)
  } catch (error) {
    state.error = String(error)
  }
  queueRender()
}

async function submitComment() {
  if (!state.currentTraceId || !state.commentDraft.trim()) return
  const selectedSpan = selectedWaterfallSpan()
  try {
    await invoke<Json>('add_comment', {
      server: state.server,
      request: {
        traceId: state.currentTraceId,
        body: state.commentDraft.trim(),
        author: 'gui',
        spanId: selectedSpan?.spanId ?? null,
      },
    })
    state.commentDraft = ''
    state.comments = parseComments(
      await invoke<Json>('get_comments', {server: state.server, traceId: state.currentTraceId}),
    )
  } catch (error) {
    state.error = String(error)
  }
  queueRender()
}

async function connect() {
  state.error = null
  state.connection = 'checking'
  state.streamId = streamId()
  queueRender()

  try {
    await invoke<string>('healthz', {server: state.server})
    state.connection = 'loading'
    await Promise.all([refreshTraces(), refreshServices(), refreshEvals()])
    await startLiveStream()
    state.connection = 'connected'
  } catch (error) {
    state.connection = 'error'
    state.error = String(error)
  }
  queueRender()
}

async function startLiveStream() {
  await invoke('start_live_stream', {
    server: state.server,
    service: state.serviceFilter || null,
    status: state.statusFilter || null,
    streamId: state.streamId,
  })
}

async function installLiveListeners() {
  liveUnlisten?.()
  liveStatusUnlisten?.()
  liveUnlisten = await listen<LivePayload>('tael://live-spans', event => {
    if (event.payload.streamId !== state.streamId || state.paused) return
    try {
      const spans = parseSpans(JSON.parse(event.payload.data))
      if (spans.length === 0) return
      updateLiveTraces(spans)
      state.spans = [...spans, ...state.spans].slice(0, MAX_LIVE_SPANS)
      state.error = null
      queueRender()
    } catch {
      // Ignore malformed events from stale streams.
    }
  })
  liveStatusUnlisten = await listen<LiveStatusPayload>('tael://live-status', event => {
    if (event.payload.streamId !== state.streamId) return
    state.connection = event.payload.status
    if (event.payload.message) state.error = event.payload.message
    queueRender()
  })
}

function selectedTrace(): LiveTraceRow | null {
  const traces = filteredLiveTraces()
  if (state.selectedTraceIdx == null) return null
  return traces[state.selectedTraceIdx] ?? null
}

function selectedSpan(): SpanRow | null {
  const spans = filteredSpans()
  if (state.selectedSpanIdx == null) return null
  return spans[state.selectedSpanIdx] ?? null
}

function selectedWaterfallSpan(): SpanRow | null {
  if (state.selectedWaterfallIdx == null) return null
  const row = state.waterfallRows[state.selectedWaterfallIdx]
  return row ? state.traceSpans[row.spanIdx] : null
}

function tabButton(tab: Tab, label: string): string {
  return `<button class="tab ${state.tab === tab ? 'active' : ''}" data-tab="${tab}">${label}</button>`
}

function render() {
  app.innerHTML = `
    <div class="shell">
      <header class="topbar">
        <div class="brand">
          <span class="brand-mark">◆</span>
          <span class="brand-name">tael</span>
          <span class="conn"><span class="conn-dot ${escapeHtml(state.connection)}"></span>${escapeHtml(state.connection)}</span>
        </div>
        <div class="conn-controls">
          <label class="field"><span>server</span><input id="server-input" class="server-input" value="${escapeHtml(state.server)}" /></label>
          <label class="field"><span>service</span><input id="service-input" class="small-input" placeholder="all" value="${escapeHtml(state.serviceFilter)}" /></label>
          <label class="field"><span>status</span>
            <select id="status-input" class="small-input">
              <option value="" ${state.statusFilter === '' ? 'selected' : ''}>all</option>
              <option value="ok" ${state.statusFilter === 'ok' ? 'selected' : ''}>ok</option>
              <option value="error" ${state.statusFilter === 'error' ? 'selected' : ''}>error</option>
            </select>
          </label>
          <label class="field"><span>window</span><input id="last-input" class="tiny-input" value="${escapeHtml(state.lastWindow)}" /></label>
          <button id="connect-btn" class="primary">Connect</button>
          <button id="refresh-btn" title="Refresh">Refresh</button>
          <button id="pause-btn" class="${state.paused ? 'active' : ''}" title="Pause live ingest">${state.paused ? 'Resume' : 'Pause'}</button>
        </div>
      </header>
      <nav class="subnav">
        <div class="tabs">
          ${tabButton('traces', 'Traces')}
          ${tabButton('services', 'Services')}
          ${tabButton('evals', 'Evals')}
          ${tabButton('timeline', 'Timeline')}
          ${state.tab === 'detail' ? tabButton('detail', 'Trace') : ''}
        </div>
        <div class="filter-box">
          <input id="filter-input" placeholder="filter…" value="${escapeHtml(state.textFilter)}" />
          ${state.textFilter ? '<button id="clear-filter-btn">Clear</button>' : ''}
        </div>
      </nav>
      ${state.error ? `<div class="error-bar">${escapeHtml(state.error)}</div>` : '<div class="error-bar is-hidden"></div>'}
      <main class="workspace">${renderTab()}</main>
      ${state.attrPickerOpen ? renderAttrPicker() : ''}
      ${state.spanViewer ? renderSpanViewer(state.spanViewer) : ''}
    </div>
  `
  bindShell()
  renderCanvases()
}

function renderTab(): string {
  if (state.tab === 'services') return renderServices()
  if (state.tab === 'evals') return renderEvals()
  if (state.tab === 'timeline') return renderTimeline()
  if (state.tab === 'detail') return renderDetail()
  return renderTraces()
}

function renderTraces(): string {
  const spans = filteredSpans()
  const selected = selectedSpan()
  const pinnedHeaders = state.pinnedColumns.map(key => `<th>${escapeHtml(key)}</th>`).join('')
  return `
    <section class="split vertical">
      <div class="pane table-pane">
        <div class="pane-title">
          <span>Traces</span>
          <span>${spans.length}/${state.spans.length}</span>
        </div>
        <div class="table-wrap">
          <table>
            <thead><tr><th>Time</th><th>Service</th><th>Operation</th><th>Duration</th><th>Status</th><th>Trace ID</th>${pinnedHeaders}</tr></thead>
            <tbody>
              ${spans.map((span, idx) => `
                <tr class="${state.selectedSpanIdx === idx ? 'selected' : ''}" data-span-idx="${idx}">
                  <td class="muted">${escapeHtml(shortTime(span.startTime))}</td>
                  <td style="color:${serviceColor(span.service)}">${escapeHtml(span.service)}</td>
                  <td>${escapeHtml(span.operation)}</td>
                  <td class="${durationClass(span.durationMs)}">${span.durationMs.toFixed(0)}ms</td>
                  <td class="${statusClass(span.status)}">${escapeHtml(span.status)}</td>
                  <td class="mono muted">${escapeHtml(shortId(span.traceId))}</td>
                  ${state.pinnedColumns.map(key => {
                    const value = attrValue(span, key)
                    return `<td class="${value ? 'attr-cell' : 'muted'}">${escapeHtml(value || '-')}</td>`
                  }).join('')}
                </tr>
              `).join('')}
            </tbody>
          </table>
        </div>
      </div>
      <aside class="pane detail-pane">${selected ? renderSpanProperties(selected) : '<div class="empty">No span selected.</div>'}</aside>
    </section>
  `
}

function renderSpanProperties(span: SpanRow): string {
  return `
    <div class="pane-title">
      <span>Span</span>
      <div class="button-row">
        <button id="pin-columns-btn">Columns</button>
        <button id="view-span-btn">View</button>
        <button id="open-selected-trace-btn">Open Trace</button>
      </div>
    </div>
    <dl class="properties">
      <dt>trace_id</dt><dd class="mono">${escapeHtml(span.traceId)}</dd>
      <dt>span_id</dt><dd class="mono">${escapeHtml(span.spanId)}</dd>
      <dt>parent</dt><dd class="mono">${escapeHtml(span.parentSpanId ?? 'none')}</dd>
      <dt>service</dt><dd style="color:${serviceColor(span.service)}">${escapeHtml(span.service)}</dd>
      <dt>operation</dt><dd>${escapeHtml(span.operation)}</dd>
      <dt>status</dt><dd class="${statusClass(span.status)}">${escapeHtml(span.status)}</dd>
      <dt>duration</dt><dd class="${durationClass(span.durationMs)}">${span.durationMs.toFixed(2)}ms</dd>
      <dt>start</dt><dd>${escapeHtml(span.startTime)}</dd>
    </dl>
    <pre class="json-view">${escapeHtml(JSON.stringify({attributes: span.attributes, events: span.events}, null, 2))}</pre>
  `
}

function renderServices(): string {
  return `
    <section class="pane table-pane full">
      <div class="pane-title"><span>Services</span><span>${state.services.length}</span></div>
      <div class="table-wrap">
        <table>
          <thead><tr><th>Service</th><th>Spans</th><th>Traces</th><th>Avg Duration</th><th>Error Rate</th></tr></thead>
          <tbody>
            ${state.services.map((service, idx) => `
              <tr class="${state.selectedServiceIdx === idx ? 'selected' : ''}" data-service-idx="${idx}">
                <td style="color:${serviceColor(service.name)}">${escapeHtml(service.name)}</td>
                <td>${service.spanCount}</td>
                <td>${service.traceCount}</td>
                <td class="${durationClass(service.avgDurationMs)}">${service.avgDurationMs.toFixed(1)}ms</td>
                <td class="${service.errorRate > 0.05 ? 'danger' : service.errorRate > 0 ? 'warn' : 'ok'}">${(service.errorRate * 100).toFixed(1)}%</td>
              </tr>
            `).join('')}
          </tbody>
        </table>
      </div>
    </section>
  `
}

function renderEvals(): string {
  const run = state.evalRun
  const cases = filteredEvalCases()
  const selected = state.selectedEvalIdx == null ? null : cases[state.selectedEvalIdx]
  if (!run) {
    return '<section class="pane full"><div class="empty">No eval runs found.</div></section>'
  }
  const correctness = typeof run.avgScores.correctness === 'number' ? run.avgScores.correctness.toFixed(3) : '-'
  return `
    <section class="split vertical eval-layout">
      <div class="pane run-strip">
        <div class="run-stat grow">
          <span class="run-stat-label">Suite</span>
          <span class="run-stat-value">${escapeHtml(run.suiteId)}</span>
          <span class="run-stat-sub mono">${escapeHtml(run.runId)}</span>
        </div>
        <div class="run-stat">
          <span class="run-stat-label">Status</span>
          <span class="run-stat-value ${statusClass(run.status)}">${escapeHtml(run.status)}</span>
        </div>
        <div class="run-stat">
          <span class="run-stat-label">Cases</span>
          <span class="run-stat-value">${run.observedCases}<span class="run-stat-sub"> / ${run.caseCount ?? '?'}</span></span>
        </div>
        <div class="run-stat">
          <span class="run-stat-label">Pass</span>
          <span class="run-stat-value ok">${run.passedCases}</span>
        </div>
        <div class="run-stat">
          <span class="run-stat-label">Fail</span>
          <span class="run-stat-value ${run.failedCases > 0 ? 'danger' : ''}">${run.failedCases}</span>
        </div>
        <div class="run-stat">
          <span class="run-stat-label">Avg score</span>
          <span class="run-stat-value">${correctness}</span>
        </div>
        <div class="run-stat">
          <span class="run-stat-label">Cost</span>
          <span class="run-stat-value">$${run.costUsd.toFixed(4)}</span>
        </div>
        <button id="failures-only-btn" class="spacer ${state.evalFailuresOnly ? 'active' : ''}">Failures</button>
      </div>
      <div class="pane table-pane">
        <div class="pane-title"><span>Cases</span><span>${cases.length}</span></div>
        <div class="table-wrap">
          <table>
            <thead><tr><th>Status</th><th>Case</th><th>Score</th><th>Cost</th><th>Duration</th><th>Trace</th></tr></thead>
            <tbody>
              ${cases.map((item, idx) => {
                const score = typeof item.scores.correctness === 'number'
                  ? item.scores.correctness.toFixed(3)
                  : Object.values(item.scores).find(v => typeof v === 'number')?.toString() ?? '-'
                return `
                  <tr class="${state.selectedEvalIdx === idx ? 'selected' : ''}" data-eval-idx="${idx}">
                    <td class="${statusClass(item.status)}">${escapeHtml(item.status.toUpperCase())}</td>
                    <td>${escapeHtml(item.caseId)}</td>
                    <td>${escapeHtml(score)}</td>
                    <td>${item.costUsd.toFixed(4)}</td>
                    <td>${item.durationMs == null ? '-' : `${item.durationMs.toFixed(0)}ms`}</td>
                    <td class="mono muted">${escapeHtml(item.traceId ? shortId(item.traceId, 12) : '-')}</td>
                  </tr>
                `
              }).join('')}
            </tbody>
          </table>
        </div>
      </div>
      <aside class="pane detail-pane">${selected ? renderEvalDetail(selected) : '<div class="empty">No case selected.</div>'}</aside>
    </section>
  `
}

function renderEvalDetail(item: EvalCaseRow): string {
  return `
    <div class="pane-title">
      <span>${escapeHtml(item.caseId)}</span>
      ${item.traceId ? '<button id="open-eval-trace-btn">Open Trace</button>' : ''}
    </div>
    <dl class="properties">
      <dt>status</dt><dd class="${statusClass(item.status)}">${escapeHtml(item.status)}</dd>
      <dt>trace</dt><dd class="mono">${escapeHtml(item.traceId ?? '-')}</dd>
      <dt>duration</dt><dd>${item.durationMs == null ? '-' : `${item.durationMs.toFixed(1)}ms`}</dd>
      <dt>cost</dt><dd>$${item.costUsd.toFixed(4)}</dd>
    </dl>
    <pre class="json-view">${escapeHtml(JSON.stringify(item.scores, null, 2))}</pre>
    ${item.comments.length ? `<div class="comment-list">${item.comments.map(renderComment).join('')}</div>` : ''}
  `
}

function renderTimeline(): string {
  const selected = selectedTrace()
  return `
    <section class="split vertical">
      <div class="pane timeline-pane">
        <div class="pane-title">
          <span>Live Timeline</span>
          <span>${filteredLiveTraces().length}/${state.liveTraces.length} traces</span>
        </div>
        <canvas id="timeline-canvas" class="timeline-canvas"></canvas>
      </div>
      <aside class="pane detail-pane">
        ${selected ? `
          <div class="pane-title"><span>Trace</span><button id="open-selected-live-trace-btn">Open Trace</button></div>
          <dl class="properties">
            <dt>trace_id</dt><dd class="mono">${escapeHtml(selected.traceId)}</dd>
            <dt>service</dt><dd style="color:${serviceColor(selected.service)}">${escapeHtml(selected.service)}</dd>
            <dt>operation</dt><dd>${escapeHtml(selected.operation)}</dd>
            <dt>status</dt><dd class="${selected.hasError ? 'danger' : 'ok'}">${selected.hasError ? 'error' : 'ok'}</dd>
            <dt>duration</dt><dd class="${durationClass(selected.durationMs)}">${selected.durationMs.toFixed(2)}ms</dd>
            <dt>spans</dt><dd>${selected.spanCount}</dd>
          </dl>
        ` : '<div class="empty">No trace selected.</div>'}
      </aside>
    </section>
  `
}

function renderDetail(): string {
  const span = selectedWaterfallSpan()
  return `
    <section class="detail-grid">
      <div class="pane waterfall-pane">
        <div class="pane-title">
          <span>${escapeHtml(state.currentTraceId ? `Trace ${shortId(state.currentTraceId)}` : 'Trace')}</span>
          <button id="back-btn">Back</button>
        </div>
        <canvas id="waterfall-canvas" class="waterfall-canvas"></canvas>
      </div>
      <aside class="pane span-side">
        ${span ? renderSpanProperties(span) : '<div class="empty">No span selected.</div>'}
      </aside>
      <section class="pane comments-pane">
        <div class="pane-title"><span>Comments</span><span>${state.comments.length}</span></div>
        <div class="comment-list">${state.comments.map(renderComment).join('') || '<div class="empty compact">No comments.</div>'}</div>
        <div class="comment-form">
          <input id="comment-input" value="${escapeHtml(state.commentDraft)}" />
          <button id="submit-comment-btn">Add</button>
        </div>
      </section>
    </section>
  `
}

function renderComment(comment: CommentRow): string {
  const time = shortTime(comment.createdAt).slice(0, 8)
  return `
    <div class="comment">
      <span class="muted">${escapeHtml(time)}</span>
      <strong>${escapeHtml(comment.author)}</strong>
      ${comment.spanId ? `<span class="mono muted">${escapeHtml(shortId(comment.spanId, 8))}</span>` : ''}
      <p>${escapeHtml(comment.body)}</p>
    </div>
  `
}

function renderAttrPicker(): string {
  const keys = attributeKeys()
  return `
    <div class="overlay">
      <section class="modal attr-modal">
        <div class="modal-title">
          <span>Pin Attribute Columns</span>
          <button id="close-attr-picker-btn">Close</button>
        </div>
        <div class="modal-body">
          ${
            keys.length
              ? keys.map(key => `
                <label class="check-row">
                  <input type="checkbox" data-attr-key="${escapeHtml(key)}" ${state.pinnedColumns.includes(key) ? 'checked' : ''} />
                  <span class="mono">${escapeHtml(key)}</span>
                </label>
              `).join('')
              : '<div class="empty compact">No attributes found.</div>'
          }
        </div>
      </section>
    </div>
  `
}

function renderSpanViewer(span: SpanRow): string {
  return `
    <div class="overlay">
      <section class="modal span-modal">
        <div class="modal-title">
          <span>${escapeHtml(span.service)} / ${escapeHtml(span.operation)}</span>
          <button id="close-span-viewer-btn">Close</button>
        </div>
        <div class="modal-body split-modal">
          <dl class="properties modal-properties">
            <dt>trace_id</dt><dd class="mono">${escapeHtml(span.traceId)}</dd>
            <dt>span_id</dt><dd class="mono">${escapeHtml(span.spanId)}</dd>
            <dt>parent</dt><dd class="mono">${escapeHtml(span.parentSpanId ?? 'none')}</dd>
            <dt>service</dt><dd style="color:${serviceColor(span.service)}">${escapeHtml(span.service)}</dd>
            <dt>operation</dt><dd>${escapeHtml(span.operation)}</dd>
            <dt>status</dt><dd class="${statusClass(span.status)}">${escapeHtml(span.status)}</dd>
            <dt>duration</dt><dd class="${durationClass(span.durationMs)}">${span.durationMs.toFixed(2)}ms</dd>
            <dt>start</dt><dd>${escapeHtml(span.startTime)}</dd>
          </dl>
          <pre class="json-view modal-json">${escapeHtml(JSON.stringify({attributes: span.attributes, events: span.events}, null, 2))}</pre>
        </div>
      </section>
    </div>
  `
}

function bindShell() {
  app.querySelector<HTMLInputElement>('#server-input')?.addEventListener('change', event => {
    state.server = (event.currentTarget as HTMLInputElement).value.trim()
  })
  app.querySelector<HTMLInputElement>('#service-input')?.addEventListener('change', event => {
    state.serviceFilter = (event.currentTarget as HTMLInputElement).value.trim()
    connect()
  })
  app.querySelector<HTMLSelectElement>('#status-input')?.addEventListener('change', event => {
    state.statusFilter = (event.currentTarget as HTMLSelectElement).value
    connect()
  })
  app.querySelector<HTMLInputElement>('#last-input')?.addEventListener('change', event => {
    state.lastWindow = (event.currentTarget as HTMLInputElement).value.trim() || '1h'
    refreshTraces().catch(error => (state.error = String(error))).finally(queueRender)
  })
  app.querySelector<HTMLInputElement>('#filter-input')?.addEventListener('input', event => {
    state.textFilter = (event.currentTarget as HTMLInputElement).value
    state.selectedSpanIdx = null
    state.selectedTraceIdx = null
    state.selectedEvalIdx = null
    queueRender()
  })
  app.querySelector('#clear-filter-btn')?.addEventListener('click', () => {
    state.textFilter = ''
    queueRender()
  })
  app.querySelector('#connect-btn')?.addEventListener('click', connect)
  app.querySelector('#refresh-btn')?.addEventListener('click', () => {
    Promise.all([refreshTraces(), refreshServices(), refreshEvals()])
      .catch(error => (state.error = String(error)))
      .finally(queueRender)
  })
  app.querySelector('#pause-btn')?.addEventListener('click', () => {
    state.paused = !state.paused
    queueRender()
  })
  app.querySelectorAll<HTMLButtonElement>('[data-tab]').forEach(button => {
    button.addEventListener('click', () => {
      state.tab = button.dataset.tab as Tab
      queueRender()
    })
  })
  app.querySelectorAll<HTMLTableRowElement>('[data-span-idx]').forEach(row => {
    row.addEventListener('click', () => {
      state.selectedSpanIdx = Number(row.dataset.spanIdx)
      queueRender()
    })
    row.addEventListener('dblclick', () => {
      const span = filteredSpans()[Number(row.dataset.spanIdx)]
      if (span) loadTrace(span.traceId)
    })
  })
  app.querySelector('#open-selected-trace-btn')?.addEventListener('click', () => {
    const span = selectedSpan() ?? selectedWaterfallSpan()
    if (span) loadTrace(span.traceId)
  })
  app.querySelector('#pin-columns-btn')?.addEventListener('click', () => {
    state.attrPickerOpen = true
    queueRender()
  })
  app.querySelector('#view-span-btn')?.addEventListener('click', () => {
    const span = selectedSpan() ?? selectedWaterfallSpan()
    if (span) {
      state.spanViewer = span
      queueRender()
    }
  })
  app.querySelector('#close-attr-picker-btn')?.addEventListener('click', () => {
    state.attrPickerOpen = false
    queueRender()
  })
  app.querySelectorAll<HTMLInputElement>('[data-attr-key]').forEach(input => {
    input.addEventListener('change', () => {
      const key = input.dataset.attrKey
      if (key) togglePinnedColumn(key)
      queueRender()
    })
  })
  app.querySelector('#close-span-viewer-btn')?.addEventListener('click', () => {
    state.spanViewer = null
    queueRender()
  })
  app.querySelectorAll<HTMLTableRowElement>('[data-service-idx]').forEach(row => {
    row.addEventListener('click', () => {
      const service = state.services[Number(row.dataset.serviceIdx)]
      if (!service) return
      state.selectedServiceIdx = Number(row.dataset.serviceIdx)
      state.serviceFilter = service.name
      state.tab = 'traces'
      connect()
    })
  })
  app.querySelector('#failures-only-btn')?.addEventListener('click', () => {
    state.evalFailuresOnly = !state.evalFailuresOnly
    state.selectedEvalIdx = null
    queueRender()
  })
  app.querySelectorAll<HTMLTableRowElement>('[data-eval-idx]').forEach(row => {
    row.addEventListener('click', () => {
      state.selectedEvalIdx = Number(row.dataset.evalIdx)
      queueRender()
    })
    row.addEventListener('dblclick', () => {
      const item = filteredEvalCases()[Number(row.dataset.evalIdx)]
      if (item?.traceId) loadTrace(item.traceId)
    })
  })
  app.querySelector('#open-eval-trace-btn')?.addEventListener('click', () => {
    const item = state.selectedEvalIdx == null ? null : filteredEvalCases()[state.selectedEvalIdx]
    if (item?.traceId) loadTrace(item.traceId)
  })
  app.querySelector('#open-selected-live-trace-btn')?.addEventListener('click', () => {
    const trace = selectedTrace()
    if (trace) loadTrace(trace.traceId)
  })
  app.querySelector('#back-btn')?.addEventListener('click', () => {
    state.tab = state.prevTab
    queueRender()
  })
  app.querySelector<HTMLInputElement>('#comment-input')?.addEventListener('input', event => {
    state.commentDraft = (event.currentTarget as HTMLInputElement).value
  })
  app.querySelector('#submit-comment-btn')?.addEventListener('click', submitComment)
}

function renderCanvases() {
  const timeline = app.querySelector<HTMLCanvasElement>('#timeline-canvas')
  if (timeline) renderTimelineCanvas(timeline)
  const waterfall = app.querySelector<HTMLCanvasElement>('#waterfall-canvas')
  if (waterfall) renderWaterfallCanvas(waterfall)
}

function prepareCanvas(canvas: HTMLCanvasElement): CanvasRenderingContext2D {
  const rect = canvas.getBoundingClientRect()
  const dpr = window.devicePixelRatio || 1
  canvas.width = Math.max(1, Math.floor(rect.width * dpr))
  canvas.height = Math.max(1, Math.floor(rect.height * dpr))
  const ctx = canvas.getContext('2d')
  if (!ctx) throw new Error('2d canvas unavailable')
  ctx.scale(dpr, dpr)
  ctx.clearRect(0, 0, rect.width, rect.height)
  return ctx
}

function renderTimelineCanvas(canvas: HTMLCanvasElement) {
  const traces = filteredLiveTraces()
  const ctx = prepareCanvas(canvas)
  const rect = canvas.getBoundingClientRect()
  const labelWidth = 260
  const rowHeight = 26
  const top = 34
  const width = Math.max(rect.width - labelWidth - 96, 1)
  const latest = traces.reduce((max, trace) => Math.max(max, trace.endTimeMs), 0)
  const baseStart = latest - state.timelineWindowMs
  const zoomStart = baseStart + state.timelineWindowMs * state.liveZoom.start
  const zoomEnd = baseStart + state.timelineWindowMs * state.liveZoom.end
  const range = Math.max(zoomEnd - zoomStart, 1)

  ctx.fillStyle = CANVAS_BG
  ctx.fillRect(0, 0, rect.width, rect.height)
  drawAxis(ctx, labelWidth, 12, width, zoomStart, zoomEnd)

  const visible = traces.filter(trace => trace.endTimeMs >= zoomStart && trace.startTimeMs <= zoomEnd)
  visible.forEach((trace, idx) => {
    const y = top + idx * rowHeight
    if (y > rect.height - rowHeight) return
    const selected = traces.indexOf(trace) === state.selectedTraceIdx
    drawRowBackground(ctx, 0, y - 3, rect.width, rowHeight, selected)
    ctx.fillStyle = serviceColor(trace.service)
    ctx.font = CANVAS_FONT
    ctx.fillText(`${trace.service} ${trace.operation}`.slice(0, 34), 18, y + 13)
    const x = labelWidth + clamp((trace.startTimeMs - zoomStart) / range, 0, 1) * width
    const w = Math.max(2, (trace.durationMs / range) * width)
    ctx.fillStyle = trace.hasError ? CANVAS_ERROR : serviceColor(trace.service)
    roundRect(ctx, x, y, Math.min(w, labelWidth + width - x), 14, 3)
    ctx.fill()
    ctx.fillStyle = CANVAS_TEXT
    ctx.fillText(`${trace.durationMs.toFixed(0)}ms`, labelWidth + width + 14, y + 12)
    ctx.fillStyle = CANVAS_FAINT
    ctx.fillText(String(trace.spanCount), labelWidth + width + 68, y + 12)
  })

  canvas.onmousemove = event => {
    const row = Math.floor((event.offsetY - top) / rowHeight)
    const visibleTrace = visible[row]
    canvas.title = visibleTrace
      ? `${visibleTrace.service} ${visibleTrace.operation} ${visibleTrace.durationMs.toFixed(1)}ms`
      : ''
  }
  canvas.onclick = event => {
    const row = Math.floor((event.offsetY - top) / rowHeight)
    const trace = visible[row]
    if (!trace) return
    state.selectedTraceIdx = traces.indexOf(trace)
    queueRender()
  }
  canvas.ondblclick = () => {
    const trace = selectedTrace()
    if (trace) loadTrace(trace.traceId)
  }
  canvas.onwheel = event => {
    event.preventDefault()
    const factor = event.deltaY > 0 ? 1.18 : 0.84
    zoomRange(state.liveZoom, factor, event.offsetX / rect.width)
    queueRender()
  }
}

function renderWaterfallCanvas(canvas: HTMLCanvasElement) {
  const ctx = prepareCanvas(canvas)
  const rect = canvas.getBoundingClientRect()
  const rows = state.waterfallRows
  const labelWidth = 300
  const rowHeight = 28
  const top = 36
  const width = Math.max(rect.width - labelWidth - 92, 1)

  ctx.fillStyle = CANVAS_BG
  ctx.fillRect(0, 0, rect.width, rect.height)
  drawAxis(ctx, labelWidth, 12, width, state.detailZoom.start, state.detailZoom.end, true)

  rows.forEach((row, idx) => {
    const span = state.traceSpans[row.spanIdx]
    const y = top + idx * rowHeight
    if (y > rect.height - rowHeight) return
    const selected = state.selectedWaterfallIdx === idx
    drawRowBackground(ctx, 0, y - 4, rect.width, rowHeight, selected)
    ctx.font = CANVAS_FONT
    ctx.fillStyle = serviceColor(span.service)
    ctx.fillText(`${' '.repeat(row.depth * 2)}${span.service} ${span.operation}`.slice(0, 42), 18, y + 13)

    const zoomWidth = state.detailZoom.end - state.detailZoom.start
    const x = labelWidth + ((row.offsetPct - state.detailZoom.start) / zoomWidth) * width
    const w = Math.max(2, (row.widthPct / zoomWidth) * width)
    if (x + w < labelWidth || x > labelWidth + width) return
    ctx.fillStyle = span.status === 'error' ? CANVAS_ERROR : serviceColor(span.service)
    roundRect(ctx, clamp(x, labelWidth, labelWidth + width), y, Math.min(w, labelWidth + width - x), 15, 3)
    ctx.fill()
    ctx.fillStyle = CANVAS_TEXT
    ctx.fillText(`${span.durationMs.toFixed(0)}ms`, labelWidth + width + 14, y + 12)
  })

  canvas.onclick = event => {
    const row = Math.floor((event.offsetY - top) / rowHeight)
    if (!rows[row]) return
    state.selectedWaterfallIdx = row
    queueRender()
  }
  canvas.ondblclick = () => {
    const span = selectedWaterfallSpan()
    if (span) state.selectedSpanIdx = state.spans.findIndex(item => item.spanId === span.spanId)
  }
  canvas.onwheel = event => {
    event.preventDefault()
    zoomRange(state.detailZoom, event.deltaY > 0 ? 1.18 : 0.84, event.offsetX / rect.width)
    queueRender()
  }
}

function drawAxis(
  ctx: CanvasRenderingContext2D,
  x: number,
  y: number,
  width: number,
  start: number,
  end: number,
  percent = false,
) {
  ctx.strokeStyle = CANVAS_AXIS
  ctx.fillStyle = CANVAS_FAINT
  ctx.font = CANVAS_AXIS_FONT
  ctx.beginPath()
  ctx.moveTo(x, y + 12)
  ctx.lineTo(x + width, y + 12)
  ctx.stroke()
  for (let i = 0; i <= 4; i += 1) {
    const px = x + (width * i) / 4
    ctx.beginPath()
    ctx.moveTo(px, y + 7)
    ctx.lineTo(px, y + 17)
    ctx.stroke()
    const value = start + ((end - start) * i) / 4
    const label = percent ? `${Math.round(value * 100)}%` : i === 4 ? 'now' : `-${Math.round((end - value) / 1000)}s`
    ctx.fillText(label, px + 4, y + 7)
  }
}

function drawRowBackground(
  ctx: CanvasRenderingContext2D,
  x: number,
  y: number,
  width: number,
  height: number,
  selected: boolean,
) {
  ctx.fillStyle = selected ? CANVAS_ROW_SELECTED : y % 56 === 0 ? CANVAS_ROW_ALT : CANVAS_BG
  ctx.fillRect(x, y, width, height)
}

function roundRect(ctx: CanvasRenderingContext2D, x: number, y: number, w: number, h: number, r: number) {
  const radius = Math.min(r, w / 2, h / 2)
  ctx.beginPath()
  ctx.moveTo(x + radius, y)
  ctx.arcTo(x + w, y, x + w, y + h, radius)
  ctx.arcTo(x + w, y + h, x, y + h, radius)
  ctx.arcTo(x, y + h, x, y, radius)
  ctx.arcTo(x, y, x + w, y, radius)
  ctx.closePath()
}

function zoomRange(range: {start: number; end: number}, factor: number, anchor: number) {
  const width = range.end - range.start
  const nextWidth = clamp(width * factor, 0.03, 1)
  const center = range.start + width * clamp(anchor, 0, 1)
  range.start = clamp(center - nextWidth * anchor, 0, 1 - nextWidth)
  range.end = range.start + nextWidth
}

window.addEventListener('keydown', event => {
  if (event.target instanceof HTMLInputElement || event.target instanceof HTMLSelectElement) return
  if (event.key === 'Escape' && state.spanViewer) {
    state.spanViewer = null
    queueRender()
    return
  }
  if (event.key === 'Escape' && state.attrPickerOpen) {
    state.attrPickerOpen = false
    queueRender()
    return
  }
  if (event.key === '1') state.tab = 'traces'
  if (event.key === '2') state.tab = 'services'
  if (event.key === '3') state.tab = 'evals'
  if (event.key === '4') state.tab = 'timeline'
  if (event.key === 'Escape' && state.tab === 'detail') state.tab = state.prevTab
  if (event.key === ' ') state.paused = !state.paused
  if (event.key === 'a' && (selectedSpan() || selectedWaterfallSpan())) state.attrPickerOpen = true
  if (event.key === 'v') {
    const span = selectedSpan() ?? selectedWaterfallSpan()
    if (span) state.spanViewer = span
  }
  queueRender()
})

window.addEventListener('resize', queueRender)

async function boot() {
  render()
  try {
    const initialServer = await invoke<string>('initial_server')
    if (initialServer.trim()) state.server = initialServer.trim()
  } catch (error) {
    console.warn('failed to load initial server', error)
  }
  try {
    await installLiveListeners()
  } catch (error) {
    state.error = `failed to install live listeners: ${String(error)}`
    queueRender()
  }
  connect()
}

boot()
refreshTimer = window.setInterval(() => {
  if (state.connection === 'connected') {
    Promise.all([refreshServices(), refreshEvals()]).catch(error => {
      state.error = String(error)
      queueRender()
    })
  }
}, 5000)

window.addEventListener('beforeunload', () => {
  liveUnlisten?.()
  liveStatusUnlisten?.()
  if (refreshTimer != null) window.clearInterval(refreshTimer)
})
