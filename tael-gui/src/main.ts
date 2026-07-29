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
  passRate: number | null
  costUsd: number
  avgScores: Record<string, Json>
  codeVersion: string | null
  startedAt: string | null
}

type EvalMetricDeltaRow = {
  metric: string
  currentAvg: number | null
  baselineAvg: number | null
  delta: number | null
  increasedCases: number
  decreasedCases: number
  unchangedCases: number
  currentOnlyCases: number
  baselineOnlyCases: number
}

type EvalCompareCaseRow = {
  caseId: string
  metric: string
  currentValue: number | null
  baselineValue: number | null
  delta: number | null
  currentTraceId: string | null
  baselineTraceId: string | null
}

type EvalCompareRow = {
  currentRunId: string
  baselineRunId: string
  currentRun: EvalRunRow | null
  baselineRun: EvalRunRow | null
  passRateDelta: number | null
  costDeltaUsd: number | null
  metrics: EvalMetricDeltaRow[]
  cases: EvalCompareCaseRow[]
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

type Tab =
  | 'traces'
  | 'services'
  | 'evals'
  | 'timeline'
  | 'detail'
  | 'health'
  | 'topology'
  | 'automation'
  | 'clusters'
  | 'review'
  | 'sql'

/// The tabs backed by `panels`, which fetch on first open rather than on
/// connect — eleven requests at startup would be paid by every session for
/// views most never open.
const PANEL_TABS = ['health', 'topology', 'automation', 'clusters', 'review', 'sql'] as const
type PanelTab = (typeof PANEL_TABS)[number]

function isPanelTab(tab: Tab): tab is PanelTab {
  return (PANEL_TABS as readonly string[]).includes(tab)
}

type ReviewRow = {
  reviewId: string
  state: 'open' | 'answered'
  traceId: string | null
  question: string
  answer: string | null
}

/// Per-panel fetch bookkeeping. A panel that failed shows why instead of an
/// empty table: SQL on a build without the engine answers with the feature to
/// install, and that message is the useful part.
type PanelState = {
  loaded: boolean
  error: string | null
  data: any
}

function emptyPanel(): PanelState {
  return {loaded: false, error: null, data: null}
}

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
  evalRuns: EvalRunRow[]
  evalSelectedRunId: string | null
  evalBaselineRunId: string | null
  evalCompare: EvalCompareRow | null
  evalCases: EvalCaseRow[]
  selectedEvalIdx: number | null
  evalFailuresOnly: boolean
  detailZoom: {start: number; end: number}
  liveZoom: {start: number; end: number}
  panels: Record<PanelTab, PanelState>
  sqlQuery: string
  suites: any[]
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
  evalRuns: [],
  evalSelectedRunId: null,
  evalBaselineRunId: null,
  evalCompare: null,
  evalCases: [],
  selectedEvalIdx: null,
  evalFailuresOnly: false,
  detailZoom: {start: 0, end: 1},
  liveZoom: {start: 0, end: 1},
  panels: {
    health: emptyPanel(),
    topology: emptyPanel(),
    automation: emptyPanel(),
    clusters: emptyPanel(),
    review: emptyPanel(),
    sql: emptyPanel(),
  },
  sqlQuery: 'SELECT service, count(*) AS spans FROM spans GROUP BY service ORDER BY spans DESC',
  suites: [],
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
  const passRate = value.pass_rate ?? value.passRate
  return {
    runId: String(value.run_id ?? value.runId ?? '-'),
    suiteId: String(value.suite_id ?? value.suiteId ?? '-'),
    status: String(value.status ?? '-'),
    caseCount: value.case_count ?? value.caseCount ?? null,
    observedCases: Number(value.observed_cases ?? value.observedCases ?? 0),
    scoredCases: Number(value.scored_cases ?? value.scoredCases ?? 0),
    passedCases: Number(value.passed_cases ?? value.passedCases ?? 0),
    failedCases: Number(value.failed_cases ?? value.failedCases ?? 0),
    passRate: typeof passRate === 'number' ? passRate : null,
    costUsd: Number(value.cost_usd ?? value.costUsd ?? 0),
    avgScores: (value.avg_scores ?? value.avgScores ?? {}) as Record<string, Json>,
    codeVersion: value.code_version ?? value.codeVersion ?? null,
    startedAt: value.started_at ?? value.startedAt ?? null,
  }
}

function optNum(value: any): number | null {
  return typeof value === 'number' && Number.isFinite(value) ? value : null
}

function parseEvalCompare(value: any): EvalCompareRow | null {
  if (!value) return null
  return {
    currentRunId: String(value.current_run_id ?? '-'),
    baselineRunId: String(value.baseline_run_id ?? '-'),
    currentRun: parseEvalRun(value.current_run),
    baselineRun: parseEvalRun(value.baseline_run),
    passRateDelta: optNum(value.pass_rate_delta),
    costDeltaUsd: optNum(value.cost_delta_usd),
    metrics: (Array.isArray(value.metrics) ? value.metrics : []).map((m: any) => ({
      metric: String(m.metric ?? '-'),
      currentAvg: optNum(m.current_avg),
      baselineAvg: optNum(m.baseline_avg),
      delta: optNum(m.delta),
      increasedCases: Number(m.increased_cases ?? 0),
      decreasedCases: Number(m.decreased_cases ?? 0),
      unchangedCases: Number(m.unchanged_cases ?? 0),
      currentOnlyCases: Number(m.current_only_cases ?? 0),
      baselineOnlyCases: Number(m.baseline_only_cases ?? 0),
    })),
    cases: (Array.isArray(value.cases) ? value.cases : []).map((c: any) => ({
      caseId: String(c.case_id ?? '-'),
      metric: String(c.metric ?? '-'),
      currentValue: optNum(c.current_value),
      baselineValue: optNum(c.baseline_value),
      delta: optNum(c.delta),
      currentTraceId: c.current_trace_id ?? null,
      baselineTraceId: c.baseline_trace_id ?? null,
    })),
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
  state.evalRuns = (Array.isArray(runsValue?.runs) ? runsValue.runs : [])
    .map(parseEvalRun)
    .filter((run: EvalRunRow | null): run is EvalRunRow => run != null)

  // Keep the user's picks across refreshes; fall back to the newest run, and
  // drop a baseline whose run no longer exists.
  const selected =
    state.evalRuns.find(run => run.runId === state.evalSelectedRunId) ?? state.evalRuns[0] ?? null
  state.evalSelectedRunId = selected?.runId ?? null
  if (!state.evalRuns.some(run => run.runId === state.evalBaselineRunId)) {
    state.evalBaselineRunId = null
  }
  if (!selected) {
    state.evalRun = null
    state.evalCases = []
    state.evalCompare = null
    return
  }

  state.evalRun = selected
  state.evalCases = parseEvalCases(
    await invoke<Json>('eval_cases', {server: state.server, runId: selected.runId}),
  )
  if (state.evalBaselineRunId && state.evalBaselineRunId !== selected.runId) {
    state.evalCompare = parseEvalCompare(
      await invoke<Json>('eval_compare', {
        server: state.server,
        runId: selected.runId,
        baseline: state.evalBaselineRunId,
      }),
    )
  } else {
    state.evalCompare = null
  }
}

// ── Panels ──────────────────────────────────────────────────────────

/// Baseline window for the health comparison: four times the current one, the
/// same shape `tael anomalies` defaults to. Falls back to the window itself
/// when it cannot be parsed, so a comparison finds nothing rather than
/// silently comparing against the wrong span of time.
function multiplyWindow(window: string, factor: number): string {
  const match = /^(\d+)([a-z]+)$/i.exec(window.trim())
  if (!match) return window
  return `${Number(match[1]) * factor}${match[2]}`
}

/// Derive the review queue from trace comments.
///
/// Requests and answers are both structured comments and an answer references
/// its request rather than mutating it, because comments are append-only. So
/// the effective state is computed here exactly as `tael review list` computes
/// it, never read from a field.
function parseReviews(value: any): ReviewRow[] {
  const bodies: any[] = (Array.isArray(value?.comments) ? value.comments : [])
    .map((comment: any) => {
      try {
        return JSON.parse(String(comment?.body ?? ''))
      } catch {
        return null
      }
    })
    .filter((body: any) => body && typeof body === 'object')

  const answers = new Map<string, any>()
  for (const body of bodies) {
    if (body.kind === 'review_answer') answers.set(String(body.review_id ?? ''), body)
  }

  const reviews: ReviewRow[] = bodies
    .filter(body => body.kind === 'review_request')
    .map(body => {
      const reviewId = String(body.review_id ?? '')
      const answer = answers.get(reviewId)
      return {
        reviewId,
        state: answer ? 'answered' : 'open',
        traceId: body.trace_id ? String(body.trace_id) : null,
        question: String(body.question ?? ''),
        answer: answer ? String(answer.answer ?? '') : null,
      }
    })
  // Open questions first: they are the ones that need a human.
  reviews.sort((a, b) =>
    a.state === b.state ? a.reviewId.localeCompare(b.reviewId) : a.state === 'open' ? -1 : 1,
  )
  return reviews
}

async function refreshPanel(tab: PanelTab) {
  const panel = state.panels[tab]
  panel.loaded = true
  panel.error = null
  const server = state.server
  const last = state.lastWindow || '1h'
  try {
    if (tab === 'health') {
      const [summary, anomalies] = await Promise.all([
        invoke<Json>('query_summary', {server, last}),
        invoke<Json>('query_anomalies', {server, last, baseline: multiplyWindow(last, 4)}),
      ])
      panel.data = {summary, anomalies}
    } else if (tab === 'topology') {
      panel.data = await invoke<Json>('query_topology', {server, last})
    } else if (tab === 'automation') {
      const [alerts, events, scoreRules] = await Promise.all([
        invoke<Json>('list_alerts', {server}),
        invoke<Json>('alert_events', {server, limit: 20}),
        invoke<Json>('list_score_rules', {server}),
      ])
      panel.data = {alerts, events, scoreRules}
    } else if (tab === 'clusters') {
      panel.data = await invoke<Json>('cluster_traces', {server, k: 5})
    } else if (tab === 'review') {
      panel.data = parseReviews(await invoke<Json>('list_comments', {server, limit: 500}))
    } else if (tab === 'sql') {
      panel.data = await invoke<Json>('query_sql', {server, query: state.sqlQuery})
    }
  } catch (error) {
    panel.error = String(error)
    panel.data = null
  }
  queueRender()
}

async function openPanel(tab: PanelTab) {
  state.tab = tab
  queueRender()
  if (!state.panels[tab].loaded) await refreshPanel(tab)
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
          ${tabButton('health', 'Health')}
          ${tabButton('topology', 'Topology')}
          ${tabButton('automation', 'Automation')}
          ${tabButton('clusters', 'Clusters')}
          ${tabButton('review', 'Review')}
          ${tabButton('sql', 'SQL')}
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
  if (isPanelTab(state.tab)) return renderPanel(state.tab)
  return renderTraces()
}

// ── Panel rendering ─────────────────────────────────────────────────

function panelNote(title: string, body: string, tone = 'muted'): string {
  return `
    <section class="pane table-pane full">
      <div class="pane-title"><span>${escapeHtml(title)}</span></div>
      <p class="panel-note ${tone}">${escapeHtml(body)}</p>
    </section>
  `
}

function renderPanel(tab: PanelTab): string {
  const panel = state.panels[tab]
  if (panel.error) return panelNote(tab, panel.error, 'danger')
  if (!panel.loaded) return panelNote(tab, 'Loading…')
  if (tab === 'health') return renderHealth(panel.data)
  if (tab === 'topology') return renderTopology(panel.data)
  if (tab === 'automation') return renderAutomation(panel.data)
  if (tab === 'clusters') return renderClusters(panel.data)
  if (tab === 'review') return renderReview(panel.data)
  return renderSql(panel.data)
}

function num(value: any, key: string): number {
  const raw = value?.[key]
  return typeof raw === 'number' && Number.isFinite(raw) ? raw : 0
}

function str(value: any, key: string): string {
  const raw = value?.[key]
  if (raw == null) return ''
  return typeof raw === 'string' ? raw : String(raw)
}

function arr(value: any, key: string): any[] {
  return Array.isArray(value?.[key]) ? value[key] : []
}

function rateClass(rate: number): string {
  if (rate > 0.05) return 'danger'
  if (rate > 0) return 'warn'
  return 'ok'
}

function statCard(label: string, value: string, tone = ''): string {
  return `
    <div class="stat">
      <span class="stat-label">${escapeHtml(label)}</span>
      <span class="stat-value ${tone}">${escapeHtml(value)}</span>
    </div>
  `
}

function renderHealth(data: any): string {
  const summary = data?.summary
  if (!summary) return panelNote('Health', 'No summary yet.')
  const traces = summary.traces ?? {}
  const logs = summary.logs ?? {}
  const errorRate = num(traces, 'error_rate')
  const anomalies = arr(data.anomalies, 'anomalies')

  const errorOps = arr(summary, 'top_error_operations')
    .slice(0, 5)
    .map(
      op => `<tr>
        <td class="danger">${escapeHtml(num(op, 'error_count'))}</td>
        <td class="accent">${escapeHtml(str(op, 'service'))}</td>
        <td>${escapeHtml(str(op, 'operation'))}</td>
      </tr>`,
    )
    .join('')

  const anomalyRows = anomalies
    .map(
      a => `<tr>
        <td class="accent">${escapeHtml(str(a, 'service'))}</td>
        <td>${escapeHtml(str(a, 'kind'))}</td>
        <td class="${str(a, 'severity') === 'high' ? 'danger' : 'warn'}">${escapeHtml(str(a, 'severity'))}</td>
        <td>${num(a, 'baseline').toFixed(2)}</td>
        <td>${num(a, 'current').toFixed(2)}</td>
        <td>${escapeHtml(str(a, 'description'))}</td>
      </tr>`,
    )
    .join('')

  return `
    <section class="pane table-pane full">
      <div class="pane-title"><span>Health</span><span>last ${escapeHtml(state.lastWindow || '1h')}</span></div>
      <div class="stat-row">
        ${statCard('spans', String(num(traces, 'span_count')))}
        ${statCard('traces', String(num(traces, 'trace_count')))}
        ${statCard('errors', String(num(traces, 'error_count')), rateClass(errorRate))}
        ${statCard('error rate', `${(errorRate * 100).toFixed(2)}%`, rateClass(errorRate))}
        ${statCard('p50', `${num(traces, 'p50_ms').toFixed(1)}ms`)}
        ${statCard('p95', `${num(traces, 'p95_ms').toFixed(1)}ms`)}
        ${statCard('p99', `${num(traces, 'p99_ms').toFixed(1)}ms`)}
        ${statCard('logs', `${num(logs, 'total')} / ${num(logs, 'error')} err`)}
      </div>
      <div class="table-wrap">
        <div class="panel-subhead">Top error operations</div>
        <table>
          <thead><tr><th>Errors</th><th>Service</th><th>Operation</th></tr></thead>
          <tbody>${errorOps || '<tr><td colspan="3" class="muted">No errors in this window.</td></tr>'}</tbody>
        </table>
        <div class="panel-subhead">Anomalies vs ${escapeHtml(multiplyWindow(state.lastWindow || '1h', 4))} baseline</div>
        <table>
          <thead><tr><th>Service</th><th>Kind</th><th>Severity</th><th>Baseline</th><th>Current</th><th>Description</th></tr></thead>
          <tbody>${anomalyRows || '<tr><td colspan="6" class="ok">Nothing regressed against the baseline window.</td></tr>'}</tbody>
        </table>
      </div>
    </section>
  `
}

function renderTopology(data: any): string {
  const edges = arr(data, 'edges')
  if (edges.length === 0) {
    return panelNote(
      'Topology',
      'No parent/child edges in this window. A single-service trace has no graph.',
    )
  }
  // A span whose parent fell outside the window looks like an entry point and
  // would overstate the roots, so the count is stated rather than buried — it
  // is the number that says whether to widen the window.
  const dangling = num(data, 'spans_with_parent_outside_window')
  const rows = edges
    .map(e => {
      const rate = num(e, 'error_rate')
      return `<tr>
        <td class="accent">${escapeHtml(str(e, 'from'))}</td>
        <td class="muted">→</td>
        <td class="accent">${escapeHtml(str(e, 'to'))}</td>
        <td>${num(e, 'calls')}</td>
        <td class="${rateClass(rate)}">${num(e, 'errors')}</td>
        <td class="${rateClass(rate)}">${(rate * 100).toFixed(1)}%</td>
        <td>${num(e, 'avg_duration_ms').toFixed(1)}ms</td>
      </tr>`
    })
    .join('')

  return `
    <section class="pane table-pane full">
      <div class="pane-title">
        <span>Topology</span>
        <span>${edges.length} edges over ${num(data, 'spans_examined')} spans${
          dangling > 0 ? ` · <b class="warn">${dangling} with a parent outside the window</b>` : ''
        }</span>
      </div>
      <div class="table-wrap">
        <table>
          <thead><tr><th>From</th><th></th><th>To</th><th>Calls</th><th>Errors</th><th>Rate</th><th>Avg</th></tr></thead>
          <tbody>${rows}</tbody>
        </table>
      </div>
    </section>
  `
}

function renderAutomation(data: any): string {
  const alerts = arr(data?.alerts, 'alerts')
  const events = arr(data?.events, 'events')
  const scoreRules = arr(data?.scoreRules, 'rules')

  const alertRows = alerts
    .map(a => {
      const alertState = str(a, 'state')
      return `<tr>
        <td class="accent">${escapeHtml(str(a, 'name'))}</td>
        <td class="${alertState === 'firing' ? 'danger' : alertState === 'pending' ? 'warn' : 'ok'}">${escapeHtml(alertState)}</td>
        <td>${num(a, 'for_seconds')}s</td>
        <td>${arr(a, 'sinks').length}</td>
        <td class="mono">${escapeHtml(str(a, 'query'))}</td>
      </tr>`
    })
    .join('')

  const eventRows = events
    .map(e => {
      const evState = str(e, 'state')
      return `<tr>
        <td class="muted">${escapeHtml(shortTime(str(e, 'at')))}</td>
        <td class="accent">${escapeHtml(str(e, 'rule'))}</td>
        <td class="${evState === 'firing' ? 'danger' : 'ok'}">${escapeHtml(str(e, 'previous_state'))} → ${escapeHtml(evState)}</td>
        <td>${arr(e, 'matched').length} series</td>
      </tr>`
    })
    .join('')

  const scoreRows = scoreRules
    .map(r => {
      const status = r?.status ?? {}
      const lastError = str(status, 'last_error')
      return `<tr>
        <td class="accent">${escapeHtml(str(r, 'name'))}</td>
        <td>${(num(r, 'sample') * 100).toFixed(0)}%</td>
        <td>${num(status, 'scored')}</td>
        <td class="danger">${escapeHtml(lastError || '—')}</td>
        <td class="mono">${escapeHtml(str(r, 'command'))}</td>
      </tr>`
    })
    .join('')

  return `
    <section class="pane table-pane full">
      <div class="pane-title"><span>Automation</span><span>${alerts.length} alert rules · ${scoreRules.length} scoring rules</span></div>
      <div class="table-wrap">
        <div class="panel-subhead">Alert rules (${alerts.length})</div>
        <table>
          <thead><tr><th>Rule</th><th>State</th><th>For</th><th>Sinks</th><th>Query</th></tr></thead>
          <tbody>${alertRows || '<tr><td colspan="5" class="muted">No alert rules. Create one with <code>tael alert create</code>.</td></tr>'}</tbody>
        </table>
        <div class="panel-subhead">Alert feed (${events.length})</div>
        <table>
          <thead><tr><th>When</th><th>Rule</th><th>Transition</th><th>Matched</th></tr></thead>
          <tbody>${eventRows || '<tr><td colspan="4" class="ok">Nothing has fired.</td></tr>'}</tbody>
        </table>
        <div class="panel-subhead">Scoring rules (${scoreRules.length})</div>
        <table>
          <thead><tr><th>Rule</th><th>Sample</th><th>Scored</th><th>Last error</th><th>Command</th></tr></thead>
          <tbody>${scoreRows || '<tr><td colspan="5" class="muted">No scoring rules. Create one with <code>tael score rule create</code>.</td></tr>'}</tbody>
        </table>
      </div>
    </section>
  `
}

function renderClusters(data: any): string {
  const clusters = arr(data, 'clusters')
  if (clusters.length === 0) {
    return panelNote(
      'Clusters',
      'Nothing embedded yet. Run `tael embed --command <your embedder>` first.',
    )
  }
  const rows = clusters
    .map(c => {
      // Cohesion is the honest part of this view: a tight number means the
      // grouping is real, a loose one means read the exemplar before believing
      // it.
      const cohesion = num(c, 'cohesion')
      const tone = cohesion >= 0.85 ? 'ok' : cohesion >= 0.7 ? 'warn' : 'danger'
      const exemplar = str(c, 'exemplar')
      return `<tr data-cluster-trace="${escapeHtml(exemplar)}">
        <td class="accent">#${num(c, 'id')}</td>
        <td>${num(c, 'size')}</td>
        <td class="${tone}">${cohesion.toFixed(3)}</td>
        <td class="danger">${cohesion >= 0.7 ? '' : 'weak'}</td>
        <td class="mono muted">${escapeHtml(exemplar)}</td>
      </tr>`
    })
    .join('')

  return `
    <section class="pane table-pane full">
      <div class="pane-title">
        <span>Clusters</span>
        <span>${clusters.length} over ${num(data, 'corpus_size')} embedded traces · cohesion below 0.7 is weak</span>
      </div>
      <div class="table-wrap">
        <table>
          <thead><tr><th>Cluster</th><th>Size</th><th>Cohesion</th><th></th><th>Exemplar (click to open)</th></tr></thead>
          <tbody>${rows}</tbody>
        </table>
      </div>
    </section>
  `
}

function renderReview(reviews: ReviewRow[]): string {
  if (!reviews || reviews.length === 0) {
    return panelNote('Review queue', 'Nothing waiting on a human.', 'ok')
  }
  const open = reviews.filter(r => r.state === 'open').length
  const rows = reviews
    .map(
      r => `<tr ${r.traceId ? `data-review-trace="${escapeHtml(r.traceId)}"` : ''}>
        <td class="${r.state === 'open' ? 'warn' : 'ok'}">${escapeHtml(r.state)}</td>
        <td class="mono muted">${escapeHtml(r.traceId ? shortId(r.traceId, 12) : '—')}</td>
        <td>${escapeHtml(r.question)}</td>
        <td class="ok">${escapeHtml(r.answer ?? '')}</td>
      </tr>`,
    )
    .join('')

  return `
    <section class="pane table-pane full">
      <div class="pane-title"><span>Review queue</span><span>${open} open of ${reviews.length}</span></div>
      <div class="table-wrap">
        <table>
          <thead><tr><th>State</th><th>Trace</th><th>Question (click to open)</th><th>Answer</th></tr></thead>
          <tbody>${rows}</tbody>
        </table>
      </div>
    </section>
  `
}

function renderSql(data: any): string {
  const rows: any[] = arr(data, 'rows')
  const columns = rows.length > 0 && rows[0] && typeof rows[0] === 'object' ? Object.keys(rows[0]) : []
  const body = rows
    .map(
      row =>
        `<tr>${columns
          .map(c => {
            const cell = row?.[c]
            const value = cell == null ? '' : typeof cell === 'string' ? cell : JSON.stringify(cell)
            return `<td>${escapeHtml(value)}</td>`
          })
          .join('')}</tr>`,
    )
    .join('')

  return `
    <section class="pane table-pane full">
      <div class="pane-title"><span>SQL</span><span>${rows.length} rows</span></div>
      <div class="sql-bar">
        <textarea id="sql-input" class="sql-input" rows="3" spellcheck="false">${escapeHtml(state.sqlQuery)}</textarea>
        <button id="sql-run-btn" class="primary">Run</button>
      </div>
      <div class="table-wrap">
        <table>
          <thead><tr>${columns.map(c => `<th>${escapeHtml(c)}</th>`).join('') || '<th></th>'}</tr></thead>
          <tbody>${body || '<tr><td class="muted">No rows.</td></tr>'}</tbody>
        </table>
      </div>
    </section>
  `
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

function evalRunOption(run: EvalRunRow, selectedId: string | null): string {
  const label = `${run.runId}${run.suiteId !== '-' ? ` · ${run.suiteId}` : ''}`
  return `<option value="${escapeHtml(run.runId)}" ${run.runId === selectedId ? 'selected' : ''}>${escapeHtml(label)}</option>`
}

function renderEvals(): string {
  const run = state.evalRun
  const cases = filteredEvalCases()
  const selected = state.selectedEvalIdx == null ? null : cases[state.selectedEvalIdx]
  if (!run) {
    return '<section class="pane full"><div class="empty">No eval runs found.</div></section>'
  }
  const correctness = typeof run.avgScores.correctness === 'number' ? run.avgScores.correctness.toFixed(3) : '-'
  const comparing = state.evalCompare != null
  return `
    <section class="split vertical eval-layout ${comparing ? 'compare-layout' : ''}">
      <div class="pane run-strip">
        <div class="run-stat grow">
          <span class="run-stat-label">Run</span>
          <select id="eval-run-select">${state.evalRuns.map(item => evalRunOption(item, run.runId)).join('')}</select>
          <span class="run-stat-sub mono">${escapeHtml(run.suiteId)}${run.codeVersion ? ` @ ${escapeHtml(run.codeVersion)}` : ''}</span>
        </div>
        <div class="run-stat grow">
          <span class="run-stat-label">Compare vs</span>
          <select id="eval-baseline-select">
            <option value="">none</option>
            ${state.evalRuns
              .filter(item => item.runId !== run.runId)
              .map(item => evalRunOption(item, state.evalBaselineRunId))
              .join('')}
          </select>
          <span class="run-stat-sub">${state.evalRuns.length} runs recorded</span>
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
          <span class="run-stat-label">Pass rate</span>
          <span class="run-stat-value ${run.passRate == null ? '' : run.passRate >= 1 ? 'ok' : run.failedCases > 0 ? 'danger' : ''}">${run.passRate == null ? '-' : `${(run.passRate * 100).toFixed(0)}%`}</span>
          <span class="run-stat-sub">${run.passedCases} pass / ${run.failedCases} fail</span>
        </div>
        <div class="run-stat">
          <span class="run-stat-label">Avg score</span>
          <span class="run-stat-value">${correctness}</span>
        </div>
        <div class="run-stat">
          <span class="run-stat-label">Cost</span>
          <span class="run-stat-value">$${run.costUsd.toFixed(4)}</span>
        </div>
        ${comparing ? '' : `<button id="failures-only-btn" class="spacer ${state.evalFailuresOnly ? 'active' : ''}">Failures</button>`}
      </div>
      ${comparing ? renderEvalCompare(state.evalCompare!) : `
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
      `}
    </section>
  `
}

function signed(value: number, digits = 3): string {
  return `${value >= 0 ? '+' : ''}${value.toFixed(digits)}`
}

/// Tone for a delta where higher is better — except cost, where higher is
/// spend. Near-zero deltas stay muted so noise doesn't read as movement.
function deltaTone(metric: string, delta: number): string {
  if (Math.abs(delta) < 1e-9) return 'muted'
  const good = metric === 'cost_usd' ? delta < 0 : delta > 0
  return good ? 'ok' : 'danger'
}

function renderEvalCompare(cmp: EvalCompareRow): string {
  const current = cmp.currentRun
  const baseline = cmp.baselineRun
  const passRateText = (run: EvalRunRow | null) =>
    run?.passRate == null ? '-' : `${(run.passRate * 100).toFixed(0)}%`
  const matched = cmp.cases.filter(c => c.delta != null).length

  const deltas = cmp.metrics.filter(m => m.delta != null)
  const maxDelta = Math.max(1e-9, ...deltas.map(m => Math.abs(m.delta!)))
  const metricRows = cmp.metrics
    .map(m => {
      const barPct = m.delta == null ? 0 : (Math.abs(m.delta) / maxDelta) * 50
      const positive = (m.delta ?? 0) >= 0
      const tone = m.delta == null ? 'muted' : deltaTone(m.metric, m.delta)
      return `
        <div class="delta-row">
          <span class="delta-metric mono">${escapeHtml(m.metric)}</span>
          <span class="delta-avgs">${m.baselineAvg == null ? '-' : m.baselineAvg.toFixed(3)} → ${m.currentAvg == null ? '-' : m.currentAvg.toFixed(3)}</span>
          <div class="delta-track">
            <div class="delta-mid"></div>
            ${m.delta == null ? '' : `<div class="delta-fill ${tone}" style="${positive ? `left:50%` : `right:50%`};width:${Math.max(barPct, 0.5)}%"></div>`}
          </div>
          <span class="delta-value ${tone}">${m.delta == null ? 'n/a' : signed(m.delta)}</span>
          <span class="delta-counts muted">▲${m.increasedCases} ▼${m.decreasedCases} =${m.unchangedCases}${m.currentOnlyCases + m.baselineOnlyCases > 0 ? ` ±${m.currentOnlyCases + m.baselineOnlyCases}` : ''}</span>
        </div>
      `
    })
    .join('')

  const q = state.textFilter.trim().toLowerCase()
  const movers = cmp.cases
    .filter(c => c.delta != null && Math.abs(c.delta) > 1e-9)
    .filter(c => !q || c.caseId.toLowerCase().includes(q) || c.metric.toLowerCase().includes(q))
    .sort((a, b) => Math.abs(b.delta!) - Math.abs(a.delta!))
    .slice(0, 100)
  const moverRows = movers
    .map(c => {
      const trace = c.currentTraceId ?? c.baselineTraceId
      return `
        <tr ${trace ? `data-cmp-trace="${escapeHtml(trace)}"` : ''}>
          <td>${escapeHtml(c.caseId)}</td>
          <td class="mono">${escapeHtml(c.metric)}</td>
          <td>${c.baselineValue == null ? '-' : c.baselineValue.toFixed(3)}</td>
          <td>${c.currentValue == null ? '-' : c.currentValue.toFixed(3)}</td>
          <td class="${deltaTone(c.metric, c.delta!)}">${signed(c.delta!)}</td>
          <td class="mono muted">${escapeHtml(trace ? shortId(trace, 12) : '-')}</td>
        </tr>
      `
    })
    .join('')

  return `
    <div class="pane compare-summary">
      <div class="stat-row">
        ${statCard('pass rate', `${passRateText(current)} vs ${passRateText(baseline)}`)}
        ${statCard('pass Δ', cmp.passRateDelta == null ? '-' : `${signed(cmp.passRateDelta * 100, 1)} pts`, cmp.passRateDelta == null ? '' : deltaTone('pass', cmp.passRateDelta))}
        ${statCard('cost', `$${(current?.costUsd ?? 0).toFixed(4)} vs $${(baseline?.costUsd ?? 0).toFixed(4)}`)}
        ${statCard('cost Δ', cmp.costDeltaUsd == null ? '-' : `${signed(cmp.costDeltaUsd, 4)}`, cmp.costDeltaUsd == null ? '' : deltaTone('cost_usd', cmp.costDeltaUsd))}
        ${statCard('scored pairs', String(matched))}
      </div>
      <div class="panel-subhead">Avg score deltas vs ${escapeHtml(cmp.baselineRunId)}</div>
      <div class="delta-rows">${metricRows || '<div class="empty compact">No shared metrics between these runs.</div>'}</div>
    </div>
    <div class="pane table-pane">
      <div class="pane-title"><span>Case movements</span><span>${movers.length} of ${matched} scored pairs</span></div>
      <div class="table-wrap">
        <table>
          <thead><tr><th>Case</th><th>Metric</th><th>Baseline</th><th>Current</th><th>Delta</th><th>Trace</th></tr></thead>
          <tbody>${moverRows || '<tr><td colspan="6" class="muted">No case moved between these runs.</td></tr>'}</tbody>
        </table>
      </div>
    </div>
    <div class="pane trend-pane">
      <div class="pane-title"><span>Aggregates across runs</span><span>click a point to open that run</span></div>
      <canvas id="eval-trend-canvas" class="trend-canvas"></canvas>
    </div>
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
    if (isPanelTab(state.tab)) {
      void refreshPanel(state.tab)
      return
    }
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
      const tab = button.dataset.tab as Tab
      if (isPanelTab(tab)) {
        void openPanel(tab)
        return
      }
      state.tab = tab
      queueRender()
    })
  })

  // Refresh re-runs whichever panel is open, so the button means the same
  // thing on every tab.
  app.querySelectorAll<HTMLTableRowElement>('[data-cluster-trace]').forEach(row => {
    row.addEventListener('click', () => void loadTrace(row.dataset.clusterTrace!))
  })
  app.querySelectorAll<HTMLTableRowElement>('[data-review-trace]').forEach(row => {
    row.addEventListener('click', () => void loadTrace(row.dataset.reviewTrace!))
  })
  const sqlInput = app.querySelector<HTMLTextAreaElement>('#sql-input')
  sqlInput?.addEventListener('input', () => {
    state.sqlQuery = sqlInput.value
  })
  app.querySelector('#sql-run-btn')?.addEventListener('click', () => {
    if (!state.sqlQuery.trim()) return
    void refreshPanel('sql')
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
  const evalRefetch = () => {
    state.selectedEvalIdx = null
    refreshEvals()
      .catch(error => (state.error = String(error)))
      .finally(queueRender)
  }
  app.querySelector<HTMLSelectElement>('#eval-run-select')?.addEventListener('change', event => {
    state.evalSelectedRunId = (event.currentTarget as HTMLSelectElement).value
    evalRefetch()
  })
  app
    .querySelector<HTMLSelectElement>('#eval-baseline-select')
    ?.addEventListener('change', event => {
      state.evalBaselineRunId = (event.currentTarget as HTMLSelectElement).value || null
      evalRefetch()
    })
  app.querySelectorAll<HTMLTableRowElement>('[data-cmp-trace]').forEach(row => {
    row.addEventListener('click', () => void loadTrace(row.dataset.cmpTrace!))
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
  const trend = app.querySelector<HTMLCanvasElement>('#eval-trend-canvas')
  if (trend) renderEvalTrendCanvas(trend)
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

/// Pass rate always draws in the accent yellow; score metrics take the rest of
/// the palette in name order, so a metric keeps its color as runs come and go.
const TREND_PASS_COLOR = '#facc15'
const TREND_METRIC_COLORS = ['#62a9ff', '#52d284', '#b78cff', '#f59e8c']

type TrendSeries = {
  label: string
  color: string
  values: Array<number | null>
}

/// Aggregates per run, oldest → newest: pass rate plus the average of each
/// score metric. Cost is deliberately not a line here — dollars on a score
/// axis would need a second scale, and this chart has one.
function evalTrendSeries(runs: EvalRunRow[]): TrendSeries[] {
  const metricNames = [...new Set(runs.flatMap(run => Object.keys(run.avgScores)))]
    .filter(name => name !== 'cost_usd')
    .sort()
    .slice(0, TREND_METRIC_COLORS.length)
  const series: TrendSeries[] = [
    {label: 'pass rate', color: TREND_PASS_COLOR, values: runs.map(run => run.passRate)},
  ]
  for (const [idx, name] of metricNames.entries()) {
    series.push({
      label: name,
      color: TREND_METRIC_COLORS[idx],
      values: runs.map(run =>
        typeof run.avgScores[name] === 'number' ? (run.avgScores[name] as number) : null,
      ),
    })
  }
  return series.filter(s => s.values.some(v => v != null))
}

function trendRuns(): EvalRunRow[] {
  return [...state.evalRuns].sort((a, b) =>
    a.startedAt === b.startedAt
      ? a.runId.localeCompare(b.runId)
      : (a.startedAt ?? '') < (b.startedAt ?? '')
        ? -1
        : 1,
  )
}

function renderEvalTrendCanvas(canvas: HTMLCanvasElement) {
  const runs = trendRuns()
  const ctx = prepareCanvas(canvas)
  const rect = canvas.getBoundingClientRect()
  ctx.fillStyle = CANVAS_BG
  ctx.fillRect(0, 0, rect.width, rect.height)
  const series = evalTrendSeries(runs)
  if (runs.length === 0 || series.length === 0) {
    ctx.fillStyle = CANVAS_FAINT
    ctx.font = CANVAS_FONT
    ctx.fillText('No scored runs to chart yet.', 18, 24)
    return
  }

  const left = 46
  const right = rect.width - 150
  const top = 30
  const bottom = rect.height - 24
  const plotWidth = Math.max(right - left, 1)
  const plotHeight = Math.max(bottom - top, 1)
  const maxValue = Math.max(1, ...series.flatMap(s => s.values.filter((v): v is number => v != null)))
  const runX = (idx: number) =>
    left + (runs.length === 1 ? plotWidth / 2 : (plotWidth * idx) / (runs.length - 1))
  const valueY = (value: number) => bottom - (value / maxValue) * plotHeight

  // Recessive horizontal gridlines with value labels.
  ctx.font = CANVAS_AXIS_FONT
  for (let i = 0; i <= 4; i += 1) {
    const value = (maxValue * i) / 4
    const y = valueY(value)
    ctx.strokeStyle = CANVAS_AXIS
    ctx.beginPath()
    ctx.moveTo(left, y)
    ctx.lineTo(right, y)
    ctx.stroke()
    ctx.fillStyle = CANVAS_FAINT
    ctx.fillText(value.toFixed(2), 8, y + 4)
  }

  // Mark the two runs under comparison so the chart answers "where am I".
  const markRun = (runId: string | null, label: string) => {
    const idx = runs.findIndex(run => run.runId === runId)
    if (idx < 0) return
    const x = runX(idx)
    ctx.strokeStyle = CANVAS_FAINT
    ctx.setLineDash([3, 3])
    ctx.beginPath()
    ctx.moveTo(x, top - 4)
    ctx.lineTo(x, bottom)
    ctx.stroke()
    ctx.setLineDash([])
    ctx.fillStyle = CANVAS_TEXT
    // Near the right edge the label would run into the legend column, so it
    // flips to the left side of its marker line.
    const flip = x > right - 60
    ctx.fillText(label, flip ? x - ctx.measureText(label).width - 4 : x + 4, top + 4)
  }
  markRun(state.evalBaselineRunId, 'base')
  markRun(state.evalSelectedRunId, 'current')

  // Run ticks along the x axis: id tails when they fit, count otherwise. The
  // tail, because run ids tend to share a prefix (`run-2026-…`) and differ at
  // the end.
  ctx.fillStyle = CANVAS_FAINT
  const idTail = (runId: string) => (runId.length > 8 ? `…${runId.slice(-7)}` : runId)
  if (runs.length <= 8) {
    runs.forEach((run, idx) => ctx.fillText(idTail(run.runId), runX(idx) - 20, rect.height - 8))
  } else {
    ctx.fillText(idTail(runs[0].runId), left, rect.height - 8)
    ctx.fillText(idTail(runs[runs.length - 1].runId), right - 52, rect.height - 8)
    ctx.fillText(`${runs.length} runs`, left + plotWidth / 2 - 24, rect.height - 8)
  }

  series.forEach((s, seriesIdx) => {
    ctx.strokeStyle = s.color
    ctx.lineWidth = 2
    ctx.beginPath()
    let started = false
    s.values.forEach((value, idx) => {
      if (value == null) return
      const x = runX(idx)
      const y = valueY(value)
      if (started) ctx.lineTo(x, y)
      else ctx.moveTo(x, y)
      started = true
    })
    ctx.stroke()
    ctx.lineWidth = 1
    s.values.forEach((value, idx) => {
      if (value == null) return
      ctx.fillStyle = s.color
      ctx.beginPath()
      ctx.arc(runX(idx), valueY(value), 3.5, 0, Math.PI * 2)
      ctx.fill()
      // A 2px surface ring keeps overlapping points separable.
      ctx.strokeStyle = CANVAS_BG
      ctx.lineWidth = 2
      ctx.stroke()
      ctx.lineWidth = 1
    })
    // Legend column on the right: the swatch carries identity, text stays ink,
    // and the latest value doubles as the direct label.
    const lastIdx = s.values.reduce<number>((acc, v, idx) => (v != null ? idx : acc), -1)
    const legendY = top + 8 + seriesIdx * 16
    ctx.fillStyle = s.color
    ctx.fillRect(right + 10, legendY - 8, 8, 8)
    ctx.fillStyle = CANVAS_TEXT
    ctx.font = CANVAS_AXIS_FONT
    ctx.fillText(
      `${s.label}${lastIdx >= 0 ? ` ${s.values[lastIdx]!.toFixed(2)}` : ''}`.slice(0, 22),
      right + 22,
      legendY,
    )
  })

  const nearestRun = (offsetX: number): number => {
    let best = 0
    let bestDist = Infinity
    runs.forEach((_, idx) => {
      const dist = Math.abs(runX(idx) - offsetX)
      if (dist < bestDist) {
        bestDist = dist
        best = idx
      }
    })
    return best
  }
  canvas.onmousemove = event => {
    const run = runs[nearestRun(event.offsetX)]
    canvas.title = run
      ? `${run.runId} · pass ${run.passRate == null ? '-' : `${(run.passRate * 100).toFixed(0)}%`} · $${run.costUsd.toFixed(4)}`
      : ''
  }
  canvas.onclick = event => {
    const run = runs[nearestRun(event.offsetX)]
    if (!run || run.runId === state.evalSelectedRunId) return
    state.evalSelectedRunId = run.runId
    state.selectedEvalIdx = null
    refreshEvals()
      .catch(error => (state.error = String(error)))
      .finally(queueRender)
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
