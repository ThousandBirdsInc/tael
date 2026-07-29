(function(){const t=document.createElement("link").relList;if(t&&t.supports&&t.supports("modulepreload"))return;for(const r of document.querySelectorAll('link[rel="modulepreload"]'))n(r);new MutationObserver(r=>{for(const i of r)if(i.type==="childList")for(const d of i.addedNodes)d.tagName==="LINK"&&d.rel==="modulepreload"&&n(d)}).observe(document,{childList:!0,subtree:!0});function s(r){const i={};return r.integrity&&(i.integrity=r.integrity),r.referrerPolicy&&(i.referrerPolicy=r.referrerPolicy),r.crossOrigin==="use-credentials"?i.credentials="include":r.crossOrigin==="anonymous"?i.credentials="omit":i.credentials="same-origin",i}function n(r){if(r.ep)return;r.ep=!0;const i=s(r);fetch(r.href,i)}})();function De(e,t=!1){return window.__TAURI_INTERNALS__.transformCallback(e,t)}async function S(e,t={},s){return window.__TAURI_INTERNALS__.invoke(e,t,s)}var Se;(function(e){e.WINDOW_RESIZED="tauri://resize",e.WINDOW_MOVED="tauri://move",e.WINDOW_CLOSE_REQUESTED="tauri://close-requested",e.WINDOW_DESTROYED="tauri://destroyed",e.WINDOW_FOCUS="tauri://focus",e.WINDOW_BLUR="tauri://blur",e.WINDOW_SCALE_FACTOR_CHANGED="tauri://scale-change",e.WINDOW_THEME_CHANGED="tauri://theme-changed",e.WINDOW_CREATED="tauri://window-created",e.WINDOW_SUSPENDED="tauri://suspended",e.WINDOW_RESUMED="tauri://resumed",e.WEBVIEW_CREATED="tauri://webview-created",e.DRAG_ENTER="tauri://drag-enter",e.DRAG_OVER="tauri://drag-over",e.DRAG_DROP="tauri://drag-drop",e.DRAG_LEAVE="tauri://drag-leave"})(Se||(Se={}));async function We(e,t){window.__TAURI_EVENT_PLUGIN_INTERNALS__.unregisterListener(e,t),await S("plugin:event|unlisten",{event:e,eventId:t})}async function we(e,t,s){var n;const r=(n=void 0)!==null&&n!==void 0?n:{kind:"Any"};return S("plugin:event|listen",{event:e,target:r,handler:De(t)}).then(i=>async()=>We(e,i))}const Pe=["health","topology","automation","clusters","review","sql"];function se(e){return Pe.includes(e)}function D(){return{loaded:!1,error:null,data:null}}const Ve=200,ee=500,ue='12px "BerkeleyMono", ui-monospace, Menlo, Consolas, monospace',ae='11px "BerkeleyMono", ui-monospace, Menlo, Consolas, monospace',Z="#141414",je="#181818",Ue="#2b2611",Te="#2a2a2a",J="#b5b5b1",W="#6f6f6c",Ce="#ef4444";function Re(){return typeof crypto<"u"&&"randomUUID"in crypto?crypto.randomUUID():`${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`}const a={server:"http://127.0.0.1:7701",serviceFilter:"",statusFilter:"",lastWindow:"1h",textFilter:"",pinnedColumns:[],attrPickerOpen:!1,spanViewer:null,tab:"traces",prevTab:"traces",paused:!1,connection:"idle",error:null,streamId:Re(),spans:[],selectedSpanIdx:null,services:[],selectedServiceIdx:null,liveTraceMap:new Map,liveTraces:[],selectedTraceIdx:null,timelineWindowMs:6e4,traceSpans:[],waterfallRows:[],selectedWaterfallIdx:null,currentTraceId:null,comments:[],commentDraft:"",evalRun:null,evalRuns:[],evalSelectedRunId:null,evalBaselineRunId:null,evalCompare:null,evalCases:[],selectedEvalIdx:null,evalFailuresOnly:!1,detailZoom:{start:0,end:1},liveZoom:{start:0,end:1},panels:{health:D(),topology:D(),automation:D(),clusters:D(),review:D(),sql:D()},sqlQuery:"SELECT service, count(*) AS spans FROM spans GROUP BY service ORDER BY spans DESC",suites:[]};let ne=null,re=null,te=!1,le=null;const xe=document.querySelector("#app");if(!xe)throw new Error("missing #app");const f=xe;function h(){te||(te=!0,requestAnimationFrame(()=>{te=!1,Ae()}))}function l(e){return String(e??"").replaceAll("&","&amp;").replaceAll("<","&lt;").replaceAll(">","&gt;").replaceAll('"',"&quot;")}function Be(e){const t=Date.parse(e);return Number.isFinite(t)?t:0}function pe(e){return(Array.isArray(e)?e:Array.isArray(e?.spans)?e.spans:[]).map(s=>{const n=String(s.start_time??s.startTime??"-");return{traceId:String(s.trace_id??s.traceId??"-"),spanId:String(s.span_id??s.spanId??"-"),parentSpanId:s.parent_span_id??s.parentSpanId??null,service:String(s.service??"-"),operation:String(s.operation??"-"),durationMs:Number(s.duration_ms??s.durationMs??0),status:String(s.status??"-"),startTime:n,startTimeMs:Be(n),attributes:s.attributes&&typeof s.attributes=="object"?s.attributes:{},events:Array.isArray(s.events)?s.events:[]}})}function He(e){return(Array.isArray(e?.services)?e.services:[]).map(t=>({name:String(t.name??"-"),spanCount:Number(t.span_count??t.spanCount??0),traceCount:Number(t.trace_count??t.traceCount??0),avgDurationMs:Number(t.avg_duration_ms??t.avgDurationMs??0),errorRate:Number(t.error_rate??t.errorRate??0)}))}function ve(e){return(Array.isArray(e?.comments)?e.comments:[]).map(t=>({author:String(t.author??"-"),body:String(t.body??""),createdAt:String(t.created_at??t.createdAt??"-"),spanId:t.span_id??t.spanId??null}))}function ie(e){if(!e)return null;const t=e.pass_rate??e.passRate;return{runId:String(e.run_id??e.runId??"-"),suiteId:String(e.suite_id??e.suiteId??"-"),status:String(e.status??"-"),caseCount:e.case_count??e.caseCount??null,observedCases:Number(e.observed_cases??e.observedCases??0),scoredCases:Number(e.scored_cases??e.scoredCases??0),passedCases:Number(e.passed_cases??e.passedCases??0),failedCases:Number(e.failed_cases??e.failedCases??0),passRate:typeof t=="number"?t:null,costUsd:Number(e.cost_usd??e.costUsd??0),avgScores:e.avg_scores??e.avgScores??{},codeVersion:e.code_version??e.codeVersion??null,startedAt:e.started_at??e.startedAt??null}}function F(e){return typeof e=="number"&&Number.isFinite(e)?e:null}function Ze(e){return e?{currentRunId:String(e.current_run_id??"-"),baselineRunId:String(e.baseline_run_id??"-"),currentRun:ie(e.current_run),baselineRun:ie(e.baseline_run),passRateDelta:F(e.pass_rate_delta),costDeltaUsd:F(e.cost_delta_usd),metrics:(Array.isArray(e.metrics)?e.metrics:[]).map(t=>({metric:String(t.metric??"-"),currentAvg:F(t.current_avg),baselineAvg:F(t.baseline_avg),delta:F(t.delta),increasedCases:Number(t.increased_cases??0),decreasedCases:Number(t.decreased_cases??0),unchangedCases:Number(t.unchanged_cases??0),currentOnlyCases:Number(t.current_only_cases??0),baselineOnlyCases:Number(t.baseline_only_cases??0)})),cases:(Array.isArray(e.cases)?e.cases:[]).map(t=>({caseId:String(t.case_id??"-"),metric:String(t.metric??"-"),currentValue:F(t.current_value),baselineValue:F(t.baseline_value),delta:F(t.delta),currentTraceId:t.current_trace_id??null,baselineTraceId:t.baseline_trace_id??null}))}:null}function ze(e){return(Array.isArray(e?.cases)?e.cases:[]).map(t=>({caseId:String(t.case_id??t.caseId??"-"),status:String(t.status??"-"),traceId:t.trace_id??t.traceId??null,durationMs:t.duration_ms??t.durationMs??null,costUsd:Number(t.cost_usd??t.costUsd??0),scores:t.scores??{},comments:ve({comments:t.comments})}))}function L(e){const t=["#facc15","#62a9ff","#52d284","#b78cff","#f59e8c","#5ad1c9","#e0a3ff","#8fc4ff","#d4b483","#ff9ab0"];let s=0;for(const n of e)s=s*31+n.charCodeAt(0)>>>0;return t[s%t.length]}function Q(e){return e>=500?"danger":e>=100?"warn":"ok"}function P(e){return e==="error"||e==="fail"?"danger":e==="ok"||e==="pass"?"ok":"muted"}function fe(e){return(e.includes("T")?e.split("T")[1]:e).replace(/Z$/,"").slice(0,12)}function U(e,t=16){return e.length>t?`${e.slice(0,t)}...`:e}function Qe(e,t){const s=e.attributes[t];return s==null?"":typeof s=="string"?s:JSON.stringify(s)}function Xe(){const e=V()??q(),t=new Set,s=[],n=r=>{if(r)for(const i of Object.keys(r.attributes))t.has(i)||(t.add(i),s.push(i))};n(e);for(const r of a.spans)n(r);for(const r of a.traceSpans)n(r);return s}function Ye(e){const t=a.pinnedColumns.indexOf(e);t>=0?a.pinnedColumns.splice(t,1):a.pinnedColumns.push(e)}function he(){const e=a.textFilter.trim().toLowerCase();return e?a.spans.filter(t=>t.service.toLowerCase().includes(e)||t.operation.toLowerCase().includes(e)||t.traceId.toLowerCase().includes(e)||t.status.toLowerCase().includes(e)):a.spans}function me(){const e=a.textFilter.trim().toLowerCase();return e?a.liveTraces.filter(t=>t.service.toLowerCase().includes(e)||t.operation.toLowerCase().includes(e)||t.traceId.toLowerCase().includes(e)||(t.hasError?"error":"ok").includes(e)):a.liveTraces}function oe(){const e=a.textFilter.trim().toLowerCase();return a.evalCases.filter(t=>a.evalFailuresOnly&&t.status!=="fail"?!1:e?t.caseId.toLowerCase().includes(e)||t.status.toLowerCase().includes(e)||(t.traceId??"").toLowerCase().includes(e):!0)}function Ge(e){if(e.length===0)return[];const t=Math.min(...e.map(p=>p.startTimeMs)),s=Math.max(...e.map(p=>p.startTimeMs+p.durationMs)),n=Math.max(s-t,1),r=new Map,i="__root__";e.forEach((p,y)=>{const o=p.parentSpanId??i,$=r.get(o)??[];$.push(y),r.set(o,$)});const d=[],c=[{parent:i,depth:0}];for(;c.length>0;){const p=c.pop(),y=r.get(p.parent)??[];for(const o of[...y].reverse()){const $=e[o];d.push({spanIdx:o,depth:p.depth,offsetPct:N(($.startTimeMs-t)/n,0,1),widthPct:N($.durationMs/n,.005,1)}),c.push({parent:$.spanId,depth:p.depth+1})}}const u=new Set(d.map(p=>p.spanIdx));return e.forEach((p,y)=>{u.has(y)||d.push({spanIdx:y,depth:0,offsetPct:N((p.startTimeMs-t)/n,0,1),widthPct:N(p.durationMs/n,.005,1)})}),d}function N(e,t,s){return Math.max(t,Math.min(s,e))}function Ee(e){for(const t of e){const s=t.startTimeMs+t.durationMs,n=a.liveTraceMap.get(t.traceId);if(!n){a.liveTraceMap.set(t.traceId,{traceId:t.traceId,service:t.service,operation:t.operation,startTimeMs:t.startTimeMs,endTimeMs:s,durationMs:t.durationMs,spanCount:1,hasError:t.status==="error"});continue}n.startTimeMs=Math.min(n.startTimeMs,t.startTimeMs),n.endTimeMs=Math.max(n.endTimeMs,s),n.durationMs=n.endTimeMs-n.startTimeMs,n.spanCount+=1,n.hasError||=t.status==="error",t.parentSpanId||(n.service=t.service,n.operation=t.operation)}if(a.liveTraces=[...a.liveTraceMap.values()].sort((t,s)=>t.startTimeMs-s.startTimeMs),a.liveTraces.length>ee){const t=a.liveTraces.slice(0,a.liveTraces.length-ee);for(const s of t)a.liveTraceMap.delete(s.traceId);a.liveTraces=a.liveTraces.slice(-ee)}}async function de(){const e=await S("query_traces",{server:a.server,request:{service:a.serviceFilter||null,status:a.statusFilter||null,last:a.lastWindow||"1h",limit:200,text:a.textFilter||null}});a.spans=pe(e),Ee(a.spans)}async function be(){a.services=He(await S("list_services",{server:a.server}))}async function z(){const e=await S("eval_runs",{server:a.server});a.evalRuns=(Array.isArray(e?.runs)?e.runs:[]).map(ie).filter(s=>s!=null);const t=a.evalRuns.find(s=>s.runId===a.evalSelectedRunId)??a.evalRuns[0]??null;if(a.evalSelectedRunId=t?.runId??null,a.evalRuns.some(s=>s.runId===a.evalBaselineRunId)||(a.evalBaselineRunId=null),!t){a.evalRun=null,a.evalCases=[],a.evalCompare=null;return}a.evalRun=t,a.evalCases=ze(await S("eval_cases",{server:a.server,runId:t.runId})),a.evalBaselineRunId&&a.evalBaselineRunId!==t.runId?a.evalCompare=Ze(await S("eval_compare",{server:a.server,runId:t.runId,baseline:a.evalBaselineRunId})):a.evalCompare=null}function Me(e,t){const s=/^(\d+)([a-z]+)$/i.exec(e.trim());return s?`${Number(s[1])*t}${s[2]}`:e}function Je(e){const t=(Array.isArray(e?.comments)?e.comments:[]).map(r=>{try{return JSON.parse(String(r?.body??""))}catch{return null}}).filter(r=>r&&typeof r=="object"),s=new Map;for(const r of t)r.kind==="review_answer"&&s.set(String(r.review_id??""),r);const n=t.filter(r=>r.kind==="review_request").map(r=>{const i=String(r.review_id??""),d=s.get(i);return{reviewId:i,state:d?"answered":"open",traceId:r.trace_id?String(r.trace_id):null,question:String(r.question??""),answer:d?String(d.answer??""):null}});return n.sort((r,i)=>r.state===i.state?r.reviewId.localeCompare(i.reviewId):r.state==="open"?-1:1),n}async function ce(e){const t=a.panels[e];t.loaded=!0,t.error=null;const s=a.server,n=a.lastWindow||"1h";try{if(e==="health"){const[r,i]=await Promise.all([S("query_summary",{server:s,last:n}),S("query_anomalies",{server:s,last:n,baseline:Me(n,4)})]);t.data={summary:r,anomalies:i}}else if(e==="topology")t.data=await S("query_topology",{server:s,last:n});else if(e==="automation"){const[r,i,d]=await Promise.all([S("list_alerts",{server:s}),S("alert_events",{server:s,limit:20}),S("list_score_rules",{server:s})]);t.data={alerts:r,events:i,scoreRules:d}}else e==="clusters"?t.data=await S("cluster_traces",{server:s,k:5}):e==="review"?t.data=Je(await S("list_comments",{server:s,limit:500})):e==="sql"&&(t.data=await S("query_sql",{server:s,query:a.sqlQuery}))}catch(r){t.error=String(r),t.data=null}h()}async function Ke(e){a.tab=e,h(),a.panels[e].loaded||await ce(e)}async function k(e){a.prevTab=a.tab==="detail"?a.prevTab:a.tab,a.tab="detail",a.currentTraceId=e,a.selectedWaterfallIdx=null,a.traceSpans=[],a.waterfallRows=[],a.comments=[],a.detailZoom={start:0,end:1},a.error=null,h();try{const[t,s]=await Promise.all([S("get_trace",{server:a.server,traceId:e}),S("get_comments",{server:a.server,traceId:e})]);a.traceSpans=pe(t),a.waterfallRows=Ge(a.traceSpans),a.selectedWaterfallIdx=a.waterfallRows.length>0?0:null,a.comments=ve(s)}catch(t){a.error=String(t)}h()}async function et(){if(!a.currentTraceId||!a.commentDraft.trim())return;const e=q();try{await S("add_comment",{server:a.server,request:{traceId:a.currentTraceId,body:a.commentDraft.trim(),author:"gui",spanId:e?.spanId??null}}),a.commentDraft="",a.comments=ve(await S("get_comments",{server:a.server,traceId:a.currentTraceId}))}catch(t){a.error=String(t)}h()}async function H(){a.error=null,a.connection="checking",a.streamId=Re(),h();try{await S("healthz",{server:a.server}),a.connection="loading",await Promise.all([de(),be(),z()]),await tt(),a.connection="connected"}catch(e){a.connection="error",a.error=String(e)}h()}async function tt(){await S("start_live_stream",{server:a.server,service:a.serviceFilter||null,status:a.statusFilter||null,streamId:a.streamId})}async function st(){ne?.(),re?.(),ne=await we("tael://live-spans",e=>{if(!(e.payload.streamId!==a.streamId||a.paused))try{const t=pe(JSON.parse(e.payload.data));if(t.length===0)return;Ee(t),a.spans=[...t,...a.spans].slice(0,Ve),a.error=null,h()}catch{}}),re=await we("tael://live-status",e=>{e.payload.streamId===a.streamId&&(a.connection=e.payload.status,e.payload.message&&(a.error=e.payload.message),h())})}function $e(){const e=me();return a.selectedTraceIdx==null?null:e[a.selectedTraceIdx]??null}function V(){const e=he();return a.selectedSpanIdx==null?null:e[a.selectedSpanIdx]??null}function q(){if(a.selectedWaterfallIdx==null)return null;const e=a.waterfallRows[a.selectedWaterfallIdx];return e?a.traceSpans[e.spanIdx]:null}function E(e,t){return`<button class="tab ${a.tab===e?"active":""}" data-tab="${e}">${t}</button>`}function Ae(){f.innerHTML=`
    <div class="shell">
      <header class="topbar">
        <div class="brand">
          <span class="brand-mark">◆</span>
          <span class="brand-name">tael</span>
          <span class="conn"><span class="conn-dot ${l(a.connection)}"></span>${l(a.connection)}</span>
        </div>
        <div class="conn-controls">
          <label class="field"><span>server</span><input id="server-input" class="server-input" value="${l(a.server)}" /></label>
          <label class="field"><span>service</span><input id="service-input" class="small-input" placeholder="all" value="${l(a.serviceFilter)}" /></label>
          <label class="field"><span>status</span>
            <select id="status-input" class="small-input">
              <option value="" ${a.statusFilter===""?"selected":""}>all</option>
              <option value="ok" ${a.statusFilter==="ok"?"selected":""}>ok</option>
              <option value="error" ${a.statusFilter==="error"?"selected":""}>error</option>
            </select>
          </label>
          <label class="field"><span>window</span><input id="last-input" class="tiny-input" value="${l(a.lastWindow)}" /></label>
          <button id="connect-btn" class="primary">Connect</button>
          <button id="refresh-btn" title="Refresh">Refresh</button>
          <button id="pause-btn" class="${a.paused?"active":""}" title="Pause live ingest">${a.paused?"Resume":"Pause"}</button>
        </div>
      </header>
      <nav class="subnav">
        <div class="tabs">
          ${E("traces","Traces")}
          ${E("services","Services")}
          ${E("evals","Evals")}
          ${E("timeline","Timeline")}
          ${E("health","Health")}
          ${E("topology","Topology")}
          ${E("automation","Automation")}
          ${E("clusters","Clusters")}
          ${E("review","Review")}
          ${E("sql","SQL")}
          ${a.tab==="detail"?E("detail","Trace"):""}
        </div>
        <div class="filter-box">
          <input id="filter-input" placeholder="filter…" value="${l(a.textFilter)}" />
          ${a.textFilter?'<button id="clear-filter-btn">Clear</button>':""}
        </div>
      </nav>
      ${a.error?`<div class="error-bar">${l(a.error)}</div>`:'<div class="error-bar is-hidden"></div>'}
      <main class="workspace">${at()}</main>
      ${a.attrPickerOpen?$t():""}
      ${a.spanViewer?gt(a.spanViewer):""}
    </div>
  `,yt(),St()}function at(){return a.tab==="services"?pt():a.tab==="evals"?vt():a.tab==="timeline"?mt():a.tab==="detail"?bt():se(a.tab)?nt(a.tab):ut()}function j(e,t,s="muted"){return`
    <section class="pane table-pane full">
      <div class="pane-title"><span>${l(e)}</span></div>
      <p class="panel-note ${s}">${l(t)}</p>
    </section>
  `}function nt(e){const t=a.panels[e];return t.error?j(e,t.error,"danger"):t.loaded?e==="health"?rt(t.data):e==="topology"?lt(t.data):e==="automation"?it(t.data):e==="clusters"?ot(t.data):e==="review"?dt(t.data):ct(t.data):j(e,"Loading…")}function g(e,t){const s=e?.[t];return typeof s=="number"&&Number.isFinite(s)?s:0}function w(e,t){const s=e?.[t];return s==null?"":typeof s=="string"?s:String(s)}function M(e,t){return Array.isArray(e?.[t])?e[t]:[]}function K(e){return e>.05?"danger":e>0?"warn":"ok"}function C(e,t,s=""){return`
    <div class="stat">
      <span class="stat-label">${l(e)}</span>
      <span class="stat-value ${s}">${l(t)}</span>
    </div>
  `}function rt(e){const t=e?.summary;if(!t)return j("Health","No summary yet.");const s=t.traces??{},n=t.logs??{},r=g(s,"error_rate"),i=M(e.anomalies,"anomalies"),d=M(t,"top_error_operations").slice(0,5).map(u=>`<tr>
        <td class="danger">${l(g(u,"error_count"))}</td>
        <td class="accent">${l(w(u,"service"))}</td>
        <td>${l(w(u,"operation"))}</td>
      </tr>`).join(""),c=i.map(u=>`<tr>
        <td class="accent">${l(w(u,"service"))}</td>
        <td>${l(w(u,"kind"))}</td>
        <td class="${w(u,"severity")==="high"?"danger":"warn"}">${l(w(u,"severity"))}</td>
        <td>${g(u,"baseline").toFixed(2)}</td>
        <td>${g(u,"current").toFixed(2)}</td>
        <td>${l(w(u,"description"))}</td>
      </tr>`).join("");return`
    <section class="pane table-pane full">
      <div class="pane-title"><span>Health</span><span>last ${l(a.lastWindow||"1h")}</span></div>
      <div class="stat-row">
        ${C("spans",String(g(s,"span_count")))}
        ${C("traces",String(g(s,"trace_count")))}
        ${C("errors",String(g(s,"error_count")),K(r))}
        ${C("error rate",`${(r*100).toFixed(2)}%`,K(r))}
        ${C("p50",`${g(s,"p50_ms").toFixed(1)}ms`)}
        ${C("p95",`${g(s,"p95_ms").toFixed(1)}ms`)}
        ${C("p99",`${g(s,"p99_ms").toFixed(1)}ms`)}
        ${C("logs",`${g(n,"total")} / ${g(n,"error")} err`)}
      </div>
      <div class="table-wrap">
        <div class="panel-subhead">Top error operations</div>
        <table>
          <thead><tr><th>Errors</th><th>Service</th><th>Operation</th></tr></thead>
          <tbody>${d||'<tr><td colspan="3" class="muted">No errors in this window.</td></tr>'}</tbody>
        </table>
        <div class="panel-subhead">Anomalies vs ${l(Me(a.lastWindow||"1h",4))} baseline</div>
        <table>
          <thead><tr><th>Service</th><th>Kind</th><th>Severity</th><th>Baseline</th><th>Current</th><th>Description</th></tr></thead>
          <tbody>${c||'<tr><td colspan="6" class="ok">Nothing regressed against the baseline window.</td></tr>'}</tbody>
        </table>
      </div>
    </section>
  `}function lt(e){const t=M(e,"edges");if(t.length===0)return j("Topology","No parent/child edges in this window. A single-service trace has no graph.");const s=g(e,"spans_with_parent_outside_window"),n=t.map(r=>{const i=g(r,"error_rate");return`<tr>
        <td class="accent">${l(w(r,"from"))}</td>
        <td class="muted">→</td>
        <td class="accent">${l(w(r,"to"))}</td>
        <td>${g(r,"calls")}</td>
        <td class="${K(i)}">${g(r,"errors")}</td>
        <td class="${K(i)}">${(i*100).toFixed(1)}%</td>
        <td>${g(r,"avg_duration_ms").toFixed(1)}ms</td>
      </tr>`}).join("");return`
    <section class="pane table-pane full">
      <div class="pane-title">
        <span>Topology</span>
        <span>${t.length} edges over ${g(e,"spans_examined")} spans${s>0?` · <b class="warn">${s} with a parent outside the window</b>`:""}</span>
      </div>
      <div class="table-wrap">
        <table>
          <thead><tr><th>From</th><th></th><th>To</th><th>Calls</th><th>Errors</th><th>Rate</th><th>Avg</th></tr></thead>
          <tbody>${n}</tbody>
        </table>
      </div>
    </section>
  `}function it(e){const t=M(e?.alerts,"alerts"),s=M(e?.events,"events"),n=M(e?.scoreRules,"rules"),r=t.map(c=>{const u=w(c,"state");return`<tr>
        <td class="accent">${l(w(c,"name"))}</td>
        <td class="${u==="firing"?"danger":u==="pending"?"warn":"ok"}">${l(u)}</td>
        <td>${g(c,"for_seconds")}s</td>
        <td>${M(c,"sinks").length}</td>
        <td class="mono">${l(w(c,"query"))}</td>
      </tr>`}).join(""),i=s.map(c=>{const u=w(c,"state");return`<tr>
        <td class="muted">${l(fe(w(c,"at")))}</td>
        <td class="accent">${l(w(c,"rule"))}</td>
        <td class="${u==="firing"?"danger":"ok"}">${l(w(c,"previous_state"))} → ${l(u)}</td>
        <td>${M(c,"matched").length} series</td>
      </tr>`}).join(""),d=n.map(c=>{const u=c?.status??{},p=w(u,"last_error");return`<tr>
        <td class="accent">${l(w(c,"name"))}</td>
        <td>${(g(c,"sample")*100).toFixed(0)}%</td>
        <td>${g(u,"scored")}</td>
        <td class="danger">${l(p||"—")}</td>
        <td class="mono">${l(w(c,"command"))}</td>
      </tr>`}).join("");return`
    <section class="pane table-pane full">
      <div class="pane-title"><span>Automation</span><span>${t.length} alert rules · ${n.length} scoring rules</span></div>
      <div class="table-wrap">
        <div class="panel-subhead">Alert rules (${t.length})</div>
        <table>
          <thead><tr><th>Rule</th><th>State</th><th>For</th><th>Sinks</th><th>Query</th></tr></thead>
          <tbody>${r||'<tr><td colspan="5" class="muted">No alert rules. Create one with <code>tael alert create</code>.</td></tr>'}</tbody>
        </table>
        <div class="panel-subhead">Alert feed (${s.length})</div>
        <table>
          <thead><tr><th>When</th><th>Rule</th><th>Transition</th><th>Matched</th></tr></thead>
          <tbody>${i||'<tr><td colspan="4" class="ok">Nothing has fired.</td></tr>'}</tbody>
        </table>
        <div class="panel-subhead">Scoring rules (${n.length})</div>
        <table>
          <thead><tr><th>Rule</th><th>Sample</th><th>Scored</th><th>Last error</th><th>Command</th></tr></thead>
          <tbody>${d||'<tr><td colspan="5" class="muted">No scoring rules. Create one with <code>tael score rule create</code>.</td></tr>'}</tbody>
        </table>
      </div>
    </section>
  `}function ot(e){const t=M(e,"clusters");if(t.length===0)return j("Clusters","Nothing embedded yet. Run `tael embed --command <your embedder>` first.");const s=t.map(n=>{const r=g(n,"cohesion"),i=r>=.85?"ok":r>=.7?"warn":"danger",d=w(n,"exemplar");return`<tr data-cluster-trace="${l(d)}">
        <td class="accent">#${g(n,"id")}</td>
        <td>${g(n,"size")}</td>
        <td class="${i}">${r.toFixed(3)}</td>
        <td class="danger">${r>=.7?"":"weak"}</td>
        <td class="mono muted">${l(d)}</td>
      </tr>`}).join("");return`
    <section class="pane table-pane full">
      <div class="pane-title">
        <span>Clusters</span>
        <span>${t.length} over ${g(e,"corpus_size")} embedded traces · cohesion below 0.7 is weak</span>
      </div>
      <div class="table-wrap">
        <table>
          <thead><tr><th>Cluster</th><th>Size</th><th>Cohesion</th><th></th><th>Exemplar (click to open)</th></tr></thead>
          <tbody>${s}</tbody>
        </table>
      </div>
    </section>
  `}function dt(e){if(!e||e.length===0)return j("Review queue","Nothing waiting on a human.","ok");const t=e.filter(n=>n.state==="open").length,s=e.map(n=>`<tr ${n.traceId?`data-review-trace="${l(n.traceId)}"`:""}>
        <td class="${n.state==="open"?"warn":"ok"}">${l(n.state)}</td>
        <td class="mono muted">${l(n.traceId?U(n.traceId,12):"—")}</td>
        <td>${l(n.question)}</td>
        <td class="ok">${l(n.answer??"")}</td>
      </tr>`).join("");return`
    <section class="pane table-pane full">
      <div class="pane-title"><span>Review queue</span><span>${t} open of ${e.length}</span></div>
      <div class="table-wrap">
        <table>
          <thead><tr><th>State</th><th>Trace</th><th>Question (click to open)</th><th>Answer</th></tr></thead>
          <tbody>${s}</tbody>
        </table>
      </div>
    </section>
  `}function ct(e){const t=M(e,"rows"),s=t.length>0&&t[0]&&typeof t[0]=="object"?Object.keys(t[0]):[],n=t.map(r=>`<tr>${s.map(i=>{const d=r?.[i],c=d==null?"":typeof d=="string"?d:JSON.stringify(d);return`<td>${l(c)}</td>`}).join("")}</tr>`).join("");return`
    <section class="pane table-pane full">
      <div class="pane-title"><span>SQL</span><span>${t.length} rows</span></div>
      <div class="sql-bar">
        <textarea id="sql-input" class="sql-input" rows="3" spellcheck="false">${l(a.sqlQuery)}</textarea>
        <button id="sql-run-btn" class="primary">Run</button>
      </div>
      <div class="table-wrap">
        <table>
          <thead><tr>${s.map(r=>`<th>${l(r)}</th>`).join("")||"<th></th>"}</tr></thead>
          <tbody>${n||'<tr><td class="muted">No rows.</td></tr>'}</tbody>
        </table>
      </div>
    </section>
  `}function ut(){const e=he(),t=V(),s=a.pinnedColumns.map(n=>`<th>${l(n)}</th>`).join("");return`
    <section class="split vertical">
      <div class="pane table-pane">
        <div class="pane-title">
          <span>Traces</span>
          <span>${e.length}/${a.spans.length}</span>
        </div>
        <div class="table-wrap">
          <table>
            <thead><tr><th>Time</th><th>Service</th><th>Operation</th><th>Duration</th><th>Status</th><th>Trace ID</th>${s}</tr></thead>
            <tbody>
              ${e.map((n,r)=>`
                <tr class="${a.selectedSpanIdx===r?"selected":""}" data-span-idx="${r}">
                  <td class="muted">${l(fe(n.startTime))}</td>
                  <td style="color:${L(n.service)}">${l(n.service)}</td>
                  <td>${l(n.operation)}</td>
                  <td class="${Q(n.durationMs)}">${n.durationMs.toFixed(0)}ms</td>
                  <td class="${P(n.status)}">${l(n.status)}</td>
                  <td class="mono muted">${l(U(n.traceId))}</td>
                  ${a.pinnedColumns.map(i=>{const d=Qe(n,i);return`<td class="${d?"attr-cell":"muted"}">${l(d||"-")}</td>`}).join("")}
                </tr>
              `).join("")}
            </tbody>
          </table>
        </div>
      </div>
      <aside class="pane detail-pane">${t?ke(t):'<div class="empty">No span selected.</div>'}</aside>
    </section>
  `}function ke(e){return`
    <div class="pane-title">
      <span>Span</span>
      <div class="button-row">
        <button id="pin-columns-btn">Columns</button>
        <button id="view-span-btn">View</button>
        <button id="open-selected-trace-btn">Open Trace</button>
      </div>
    </div>
    <dl class="properties">
      <dt>trace_id</dt><dd class="mono">${l(e.traceId)}</dd>
      <dt>span_id</dt><dd class="mono">${l(e.spanId)}</dd>
      <dt>parent</dt><dd class="mono">${l(e.parentSpanId??"none")}</dd>
      <dt>service</dt><dd style="color:${L(e.service)}">${l(e.service)}</dd>
      <dt>operation</dt><dd>${l(e.operation)}</dd>
      <dt>status</dt><dd class="${P(e.status)}">${l(e.status)}</dd>
      <dt>duration</dt><dd class="${Q(e.durationMs)}">${e.durationMs.toFixed(2)}ms</dd>
      <dt>start</dt><dd>${l(e.startTime)}</dd>
    </dl>
    <pre class="json-view">${l(JSON.stringify({attributes:e.attributes,events:e.events},null,2))}</pre>
  `}function pt(){return`
    <section class="pane table-pane full">
      <div class="pane-title"><span>Services</span><span>${a.services.length}</span></div>
      <div class="table-wrap">
        <table>
          <thead><tr><th>Service</th><th>Spans</th><th>Traces</th><th>Avg Duration</th><th>Error Rate</th></tr></thead>
          <tbody>
            ${a.services.map((e,t)=>`
              <tr class="${a.selectedServiceIdx===t?"selected":""}" data-service-idx="${t}">
                <td style="color:${L(e.name)}">${l(e.name)}</td>
                <td>${e.spanCount}</td>
                <td>${e.traceCount}</td>
                <td class="${Q(e.avgDurationMs)}">${e.avgDurationMs.toFixed(1)}ms</td>
                <td class="${e.errorRate>.05?"danger":e.errorRate>0?"warn":"ok"}">${(e.errorRate*100).toFixed(1)}%</td>
              </tr>
            `).join("")}
          </tbody>
        </table>
      </div>
    </section>
  `}function Ie(e,t){const s=`${e.runId}${e.suiteId!=="-"?` · ${e.suiteId}`:""}`;return`<option value="${l(e.runId)}" ${e.runId===t?"selected":""}>${l(s)}</option>`}function vt(){const e=a.evalRun,t=oe(),s=a.selectedEvalIdx==null?null:t[a.selectedEvalIdx];if(!e)return'<section class="pane full"><div class="empty">No eval runs found.</div></section>';const n=typeof e.avgScores.correctness=="number"?e.avgScores.correctness.toFixed(3):"-",r=a.evalCompare!=null;return`
    <section class="split vertical eval-layout ${r?"compare-layout":""}">
      <div class="pane run-strip">
        <div class="run-stat grow">
          <span class="run-stat-label">Run</span>
          <select id="eval-run-select">${a.evalRuns.map(i=>Ie(i,e.runId)).join("")}</select>
          <span class="run-stat-sub mono">${l(e.suiteId)}${e.codeVersion?` @ ${l(e.codeVersion)}`:""}</span>
        </div>
        <div class="run-stat grow">
          <span class="run-stat-label">Compare vs</span>
          <select id="eval-baseline-select">
            <option value="">none</option>
            ${a.evalRuns.filter(i=>i.runId!==e.runId).map(i=>Ie(i,a.evalBaselineRunId)).join("")}
          </select>
          <span class="run-stat-sub">${a.evalRuns.length} runs recorded</span>
        </div>
        <div class="run-stat">
          <span class="run-stat-label">Status</span>
          <span class="run-stat-value ${P(e.status)}">${l(e.status)}</span>
        </div>
        <div class="run-stat">
          <span class="run-stat-label">Cases</span>
          <span class="run-stat-value">${e.observedCases}<span class="run-stat-sub"> / ${e.caseCount??"?"}</span></span>
        </div>
        <div class="run-stat">
          <span class="run-stat-label">Pass rate</span>
          <span class="run-stat-value ${e.passRate==null?"":e.passRate>=1?"ok":e.failedCases>0?"danger":""}">${e.passRate==null?"-":`${(e.passRate*100).toFixed(0)}%`}</span>
          <span class="run-stat-sub">${e.passedCases} pass / ${e.failedCases} fail</span>
        </div>
        <div class="run-stat">
          <span class="run-stat-label">Avg score</span>
          <span class="run-stat-value">${n}</span>
        </div>
        <div class="run-stat">
          <span class="run-stat-label">Cost</span>
          <span class="run-stat-value">$${e.costUsd.toFixed(4)}</span>
        </div>
        ${r?"":`<button id="failures-only-btn" class="spacer ${a.evalFailuresOnly?"active":""}">Failures</button>`}
      </div>
      ${r?ft(a.evalCompare):`
      <div class="pane table-pane">
        <div class="pane-title"><span>Cases</span><span>${t.length}</span></div>
        <div class="table-wrap">
          <table>
            <thead><tr><th>Status</th><th>Case</th><th>Score</th><th>Cost</th><th>Duration</th><th>Trace</th></tr></thead>
            <tbody>
              ${t.map((i,d)=>{const c=typeof i.scores.correctness=="number"?i.scores.correctness.toFixed(3):Object.values(i.scores).find(u=>typeof u=="number")?.toString()??"-";return`
                  <tr class="${a.selectedEvalIdx===d?"selected":""}" data-eval-idx="${d}">
                    <td class="${P(i.status)}">${l(i.status.toUpperCase())}</td>
                    <td>${l(i.caseId)}</td>
                    <td>${l(c)}</td>
                    <td>${i.costUsd.toFixed(4)}</td>
                    <td>${i.durationMs==null?"-":`${i.durationMs.toFixed(0)}ms`}</td>
                    <td class="mono muted">${l(i.traceId?U(i.traceId,12):"-")}</td>
                  </tr>
                `}).join("")}
            </tbody>
          </table>
        </div>
      </div>
      <aside class="pane detail-pane">${s?ht(s):'<div class="empty">No case selected.</div>'}</aside>
      `}
    </section>
  `}function Y(e,t=3){return`${e>=0?"+":""}${e.toFixed(t)}`}function G(e,t){return Math.abs(t)<1e-9?"muted":(e==="cost_usd"?t<0:t>0)?"ok":"danger"}function ft(e){const t=e.currentRun,s=e.baselineRun,n=o=>o?.passRate==null?"-":`${(o.passRate*100).toFixed(0)}%`,r=e.cases.filter(o=>o.delta!=null).length,i=e.metrics.filter(o=>o.delta!=null),d=Math.max(1e-9,...i.map(o=>Math.abs(o.delta))),c=e.metrics.map(o=>{const $=o.delta==null?0:Math.abs(o.delta)/d*50,R=(o.delta??0)>=0,v=o.delta==null?"muted":G(o.metric,o.delta);return`
        <div class="delta-row">
          <span class="delta-metric mono">${l(o.metric)}</span>
          <span class="delta-avgs">${o.baselineAvg==null?"-":o.baselineAvg.toFixed(3)} → ${o.currentAvg==null?"-":o.currentAvg.toFixed(3)}</span>
          <div class="delta-track">
            <div class="delta-mid"></div>
            ${o.delta==null?"":`<div class="delta-fill ${v}" style="${R?"left:50%":"right:50%"};width:${Math.max($,.5)}%"></div>`}
          </div>
          <span class="delta-value ${v}">${o.delta==null?"n/a":Y(o.delta)}</span>
          <span class="delta-counts muted">▲${o.increasedCases} ▼${o.decreasedCases} =${o.unchangedCases}${o.currentOnlyCases+o.baselineOnlyCases>0?` ±${o.currentOnlyCases+o.baselineOnlyCases}`:""}</span>
        </div>
      `}).join(""),u=a.textFilter.trim().toLowerCase(),p=e.cases.filter(o=>o.delta!=null&&Math.abs(o.delta)>1e-9).filter(o=>!u||o.caseId.toLowerCase().includes(u)||o.metric.toLowerCase().includes(u)).sort((o,$)=>Math.abs($.delta)-Math.abs(o.delta)).slice(0,100),y=p.map(o=>{const $=o.currentTraceId??o.baselineTraceId;return`
        <tr ${$?`data-cmp-trace="${l($)}"`:""}>
          <td>${l(o.caseId)}</td>
          <td class="mono">${l(o.metric)}</td>
          <td>${o.baselineValue==null?"-":o.baselineValue.toFixed(3)}</td>
          <td>${o.currentValue==null?"-":o.currentValue.toFixed(3)}</td>
          <td class="${G(o.metric,o.delta)}">${Y(o.delta)}</td>
          <td class="mono muted">${l($?U($,12):"-")}</td>
        </tr>
      `}).join("");return`
    <div class="pane compare-summary">
      <div class="stat-row">
        ${C("pass rate",`${n(t)} vs ${n(s)}`)}
        ${C("pass Δ",e.passRateDelta==null?"-":`${Y(e.passRateDelta*100,1)} pts`,e.passRateDelta==null?"":G("pass",e.passRateDelta))}
        ${C("cost",`$${(t?.costUsd??0).toFixed(4)} vs $${(s?.costUsd??0).toFixed(4)}`)}
        ${C("cost Δ",e.costDeltaUsd==null?"-":`${Y(e.costDeltaUsd,4)}`,e.costDeltaUsd==null?"":G("cost_usd",e.costDeltaUsd))}
        ${C("scored pairs",String(r))}
      </div>
      <div class="panel-subhead">Avg score deltas vs ${l(e.baselineRunId)}</div>
      <div class="delta-rows">${c||'<div class="empty compact">No shared metrics between these runs.</div>'}</div>
    </div>
    <div class="pane table-pane">
      <div class="pane-title"><span>Case movements</span><span>${p.length} of ${r} scored pairs</span></div>
      <div class="table-wrap">
        <table>
          <thead><tr><th>Case</th><th>Metric</th><th>Baseline</th><th>Current</th><th>Delta</th><th>Trace</th></tr></thead>
          <tbody>${y||'<tr><td colspan="6" class="muted">No case moved between these runs.</td></tr>'}</tbody>
        </table>
      </div>
    </div>
    <div class="pane trend-pane">
      <div class="pane-title"><span>Aggregates across runs</span><span>click a point to open that run</span></div>
      <canvas id="eval-trend-canvas" class="trend-canvas"></canvas>
    </div>
  `}function ht(e){return`
    <div class="pane-title">
      <span>${l(e.caseId)}</span>
      ${e.traceId?'<button id="open-eval-trace-btn">Open Trace</button>':""}
    </div>
    <dl class="properties">
      <dt>status</dt><dd class="${P(e.status)}">${l(e.status)}</dd>
      <dt>trace</dt><dd class="mono">${l(e.traceId??"-")}</dd>
      <dt>duration</dt><dd>${e.durationMs==null?"-":`${e.durationMs.toFixed(1)}ms`}</dd>
      <dt>cost</dt><dd>$${e.costUsd.toFixed(4)}</dd>
    </dl>
    <pre class="json-view">${l(JSON.stringify(e.scores,null,2))}</pre>
    ${e.comments.length?`<div class="comment-list">${e.comments.map(Ne).join("")}</div>`:""}
  `}function mt(){const e=$e();return`
    <section class="split vertical">
      <div class="pane timeline-pane">
        <div class="pane-title">
          <span>Live Timeline</span>
          <span>${me().length}/${a.liveTraces.length} traces</span>
        </div>
        <canvas id="timeline-canvas" class="timeline-canvas"></canvas>
      </div>
      <aside class="pane detail-pane">
        ${e?`
          <div class="pane-title"><span>Trace</span><button id="open-selected-live-trace-btn">Open Trace</button></div>
          <dl class="properties">
            <dt>trace_id</dt><dd class="mono">${l(e.traceId)}</dd>
            <dt>service</dt><dd style="color:${L(e.service)}">${l(e.service)}</dd>
            <dt>operation</dt><dd>${l(e.operation)}</dd>
            <dt>status</dt><dd class="${e.hasError?"danger":"ok"}">${e.hasError?"error":"ok"}</dd>
            <dt>duration</dt><dd class="${Q(e.durationMs)}">${e.durationMs.toFixed(2)}ms</dd>
            <dt>spans</dt><dd>${e.spanCount}</dd>
          </dl>
        `:'<div class="empty">No trace selected.</div>'}
      </aside>
    </section>
  `}function bt(){const e=q();return`
    <section class="detail-grid">
      <div class="pane waterfall-pane">
        <div class="pane-title">
          <span>${l(a.currentTraceId?`Trace ${U(a.currentTraceId)}`:"Trace")}</span>
          <button id="back-btn">Back</button>
        </div>
        <canvas id="waterfall-canvas" class="waterfall-canvas"></canvas>
      </div>
      <aside class="pane span-side">
        ${e?ke(e):'<div class="empty">No span selected.</div>'}
      </aside>
      <section class="pane comments-pane">
        <div class="pane-title"><span>Comments</span><span>${a.comments.length}</span></div>
        <div class="comment-list">${a.comments.map(Ne).join("")||'<div class="empty compact">No comments.</div>'}</div>
        <div class="comment-form">
          <input id="comment-input" value="${l(a.commentDraft)}" />
          <button id="submit-comment-btn">Add</button>
        </div>
      </section>
    </section>
  `}function Ne(e){const t=fe(e.createdAt).slice(0,8);return`
    <div class="comment">
      <span class="muted">${l(t)}</span>
      <strong>${l(e.author)}</strong>
      ${e.spanId?`<span class="mono muted">${l(U(e.spanId,8))}</span>`:""}
      <p>${l(e.body)}</p>
    </div>
  `}function $t(){const e=Xe();return`
    <div class="overlay">
      <section class="modal attr-modal">
        <div class="modal-title">
          <span>Pin Attribute Columns</span>
          <button id="close-attr-picker-btn">Close</button>
        </div>
        <div class="modal-body">
          ${e.length?e.map(t=>`
                <label class="check-row">
                  <input type="checkbox" data-attr-key="${l(t)}" ${a.pinnedColumns.includes(t)?"checked":""} />
                  <span class="mono">${l(t)}</span>
                </label>
              `).join(""):'<div class="empty compact">No attributes found.</div>'}
        </div>
      </section>
    </div>
  `}function gt(e){return`
    <div class="overlay">
      <section class="modal span-modal">
        <div class="modal-title">
          <span>${l(e.service)} / ${l(e.operation)}</span>
          <button id="close-span-viewer-btn">Close</button>
        </div>
        <div class="modal-body split-modal">
          <dl class="properties modal-properties">
            <dt>trace_id</dt><dd class="mono">${l(e.traceId)}</dd>
            <dt>span_id</dt><dd class="mono">${l(e.spanId)}</dd>
            <dt>parent</dt><dd class="mono">${l(e.parentSpanId??"none")}</dd>
            <dt>service</dt><dd style="color:${L(e.service)}">${l(e.service)}</dd>
            <dt>operation</dt><dd>${l(e.operation)}</dd>
            <dt>status</dt><dd class="${P(e.status)}">${l(e.status)}</dd>
            <dt>duration</dt><dd class="${Q(e.durationMs)}">${e.durationMs.toFixed(2)}ms</dd>
            <dt>start</dt><dd>${l(e.startTime)}</dd>
          </dl>
          <pre class="json-view modal-json">${l(JSON.stringify({attributes:e.attributes,events:e.events},null,2))}</pre>
        </div>
      </section>
    </div>
  `}function yt(){f.querySelector("#server-input")?.addEventListener("change",s=>{a.server=s.currentTarget.value.trim()}),f.querySelector("#service-input")?.addEventListener("change",s=>{a.serviceFilter=s.currentTarget.value.trim(),H()}),f.querySelector("#status-input")?.addEventListener("change",s=>{a.statusFilter=s.currentTarget.value,H()}),f.querySelector("#last-input")?.addEventListener("change",s=>{a.lastWindow=s.currentTarget.value.trim()||"1h",de().catch(n=>a.error=String(n)).finally(h)}),f.querySelector("#filter-input")?.addEventListener("input",s=>{a.textFilter=s.currentTarget.value,a.selectedSpanIdx=null,a.selectedTraceIdx=null,a.selectedEvalIdx=null,h()}),f.querySelector("#clear-filter-btn")?.addEventListener("click",()=>{a.textFilter="",h()}),f.querySelector("#connect-btn")?.addEventListener("click",H),f.querySelector("#refresh-btn")?.addEventListener("click",()=>{if(se(a.tab)){ce(a.tab);return}Promise.all([de(),be(),z()]).catch(s=>a.error=String(s)).finally(h)}),f.querySelector("#pause-btn")?.addEventListener("click",()=>{a.paused=!a.paused,h()}),f.querySelectorAll("[data-tab]").forEach(s=>{s.addEventListener("click",()=>{const n=s.dataset.tab;if(se(n)){Ke(n);return}a.tab=n,h()})}),f.querySelectorAll("[data-cluster-trace]").forEach(s=>{s.addEventListener("click",()=>{k(s.dataset.clusterTrace)})}),f.querySelectorAll("[data-review-trace]").forEach(s=>{s.addEventListener("click",()=>{k(s.dataset.reviewTrace)})});const e=f.querySelector("#sql-input");e?.addEventListener("input",()=>{a.sqlQuery=e.value}),f.querySelector("#sql-run-btn")?.addEventListener("click",()=>{a.sqlQuery.trim()&&ce("sql")}),f.querySelectorAll("[data-span-idx]").forEach(s=>{s.addEventListener("click",()=>{a.selectedSpanIdx=Number(s.dataset.spanIdx),h()}),s.addEventListener("dblclick",()=>{const n=he()[Number(s.dataset.spanIdx)];n&&k(n.traceId)})}),f.querySelector("#open-selected-trace-btn")?.addEventListener("click",()=>{const s=V()??q();s&&k(s.traceId)}),f.querySelector("#pin-columns-btn")?.addEventListener("click",()=>{a.attrPickerOpen=!0,h()}),f.querySelector("#view-span-btn")?.addEventListener("click",()=>{const s=V()??q();s&&(a.spanViewer=s,h())}),f.querySelector("#close-attr-picker-btn")?.addEventListener("click",()=>{a.attrPickerOpen=!1,h()}),f.querySelectorAll("[data-attr-key]").forEach(s=>{s.addEventListener("change",()=>{const n=s.dataset.attrKey;n&&Ye(n),h()})}),f.querySelector("#close-span-viewer-btn")?.addEventListener("click",()=>{a.spanViewer=null,h()}),f.querySelectorAll("[data-service-idx]").forEach(s=>{s.addEventListener("click",()=>{const n=a.services[Number(s.dataset.serviceIdx)];n&&(a.selectedServiceIdx=Number(s.dataset.serviceIdx),a.serviceFilter=n.name,a.tab="traces",H())})}),f.querySelector("#failures-only-btn")?.addEventListener("click",()=>{a.evalFailuresOnly=!a.evalFailuresOnly,a.selectedEvalIdx=null,h()});const t=()=>{a.selectedEvalIdx=null,z().catch(s=>a.error=String(s)).finally(h)};f.querySelector("#eval-run-select")?.addEventListener("change",s=>{a.evalSelectedRunId=s.currentTarget.value,t()}),f.querySelector("#eval-baseline-select")?.addEventListener("change",s=>{a.evalBaselineRunId=s.currentTarget.value||null,t()}),f.querySelectorAll("[data-cmp-trace]").forEach(s=>{s.addEventListener("click",()=>{k(s.dataset.cmpTrace)})}),f.querySelectorAll("[data-eval-idx]").forEach(s=>{s.addEventListener("click",()=>{a.selectedEvalIdx=Number(s.dataset.evalIdx),h()}),s.addEventListener("dblclick",()=>{const n=oe()[Number(s.dataset.evalIdx)];n?.traceId&&k(n.traceId)})}),f.querySelector("#open-eval-trace-btn")?.addEventListener("click",()=>{const s=a.selectedEvalIdx==null?null:oe()[a.selectedEvalIdx];s?.traceId&&k(s.traceId)}),f.querySelector("#open-selected-live-trace-btn")?.addEventListener("click",()=>{const s=$e();s&&k(s.traceId)}),f.querySelector("#back-btn")?.addEventListener("click",()=>{a.tab=a.prevTab,h()}),f.querySelector("#comment-input")?.addEventListener("input",s=>{a.commentDraft=s.currentTarget.value}),f.querySelector("#submit-comment-btn")?.addEventListener("click",et)}function St(){const e=f.querySelector("#timeline-canvas");e&&wt(e);const t=f.querySelector("#waterfall-canvas");t&&It(t);const s=f.querySelector("#eval-trend-canvas");s&&Rt(s)}function ge(e){const t=e.getBoundingClientRect(),s=window.devicePixelRatio||1;e.width=Math.max(1,Math.floor(t.width*s)),e.height=Math.max(1,Math.floor(t.height*s));const n=e.getContext("2d");if(!n)throw new Error("2d canvas unavailable");return n.scale(s,s),n.clearRect(0,0,t.width,t.height),n}function wt(e){const t=me(),s=ge(e),n=e.getBoundingClientRect(),r=260,i=26,d=34,c=Math.max(n.width-r-96,1),p=t.reduce((v,_)=>Math.max(v,_.endTimeMs),0)-a.timelineWindowMs,y=p+a.timelineWindowMs*a.liveZoom.start,o=p+a.timelineWindowMs*a.liveZoom.end,$=Math.max(o-y,1);s.fillStyle=Z,s.fillRect(0,0,n.width,n.height),Le(s,r,12,c,y,o);const R=t.filter(v=>v.endTimeMs>=y&&v.startTimeMs<=o);R.forEach((v,_)=>{const I=d+_*i;if(I>n.height-i)return;const m=t.indexOf(v)===a.selectedTraceIdx;Fe(s,0,I-3,n.width,i,m),s.fillStyle=L(v.service),s.font=ue,s.fillText(`${v.service} ${v.operation}`.slice(0,34),18,I+13);const b=r+N((v.startTimeMs-y)/$,0,1)*c,T=Math.max(2,v.durationMs/$*c);s.fillStyle=v.hasError?Ce:L(v.service),qe(s,b,I,Math.min(T,r+c-b),14,3),s.fill(),s.fillStyle=J,s.fillText(`${v.durationMs.toFixed(0)}ms`,r+c+14,I+12),s.fillStyle=W,s.fillText(String(v.spanCount),r+c+68,I+12)}),e.onmousemove=v=>{const _=Math.floor((v.offsetY-d)/i),I=R[_];e.title=I?`${I.service} ${I.operation} ${I.durationMs.toFixed(1)}ms`:""},e.onclick=v=>{const _=Math.floor((v.offsetY-d)/i),I=R[_];I&&(a.selectedTraceIdx=t.indexOf(I),h())},e.ondblclick=()=>{const v=$e();v&&k(v.traceId)},e.onwheel=v=>{v.preventDefault();const _=v.deltaY>0?1.18:.84;Oe(a.liveZoom,_,v.offsetX/n.width),h()}}function It(e){const t=ge(e),s=e.getBoundingClientRect(),n=a.waterfallRows,r=300,i=28,d=36,c=Math.max(s.width-r-92,1);t.fillStyle=Z,t.fillRect(0,0,s.width,s.height),Le(t,r,12,c,a.detailZoom.start,a.detailZoom.end,!0),n.forEach((u,p)=>{const y=a.traceSpans[u.spanIdx],o=d+p*i;if(o>s.height-i)return;const $=a.selectedWaterfallIdx===p;Fe(t,0,o-4,s.width,i,$),t.font=ue,t.fillStyle=L(y.service),t.fillText(`${" ".repeat(u.depth*2)}${y.service} ${y.operation}`.slice(0,42),18,o+13);const R=a.detailZoom.end-a.detailZoom.start,v=r+(u.offsetPct-a.detailZoom.start)/R*c,_=Math.max(2,u.widthPct/R*c);v+_<r||v>r+c||(t.fillStyle=y.status==="error"?Ce:L(y.service),qe(t,N(v,r,r+c),o,Math.min(_,r+c-v),15,3),t.fill(),t.fillStyle=J,t.fillText(`${y.durationMs.toFixed(0)}ms`,r+c+14,o+12))}),e.onclick=u=>{const p=Math.floor((u.offsetY-d)/i);n[p]&&(a.selectedWaterfallIdx=p,h())},e.ondblclick=()=>{const u=q();u&&(a.selectedSpanIdx=a.spans.findIndex(p=>p.spanId===u.spanId))},e.onwheel=u=>{u.preventDefault(),Oe(a.detailZoom,u.deltaY>0?1.18:.84,u.offsetX/s.width),h()}}const _t="#facc15",_e=["#62a9ff","#52d284","#b78cff","#f59e8c"];function Tt(e){const t=[...new Set(e.flatMap(n=>Object.keys(n.avgScores)))].filter(n=>n!=="cost_usd").sort().slice(0,_e.length),s=[{label:"pass rate",color:_t,values:e.map(n=>n.passRate)}];for(const[n,r]of t.entries())s.push({label:r,color:_e[n],values:e.map(i=>typeof i.avgScores[r]=="number"?i.avgScores[r]:null)});return s.filter(n=>n.values.some(r=>r!=null))}function Ct(){return[...a.evalRuns].sort((e,t)=>e.startedAt===t.startedAt?e.runId.localeCompare(t.runId):(e.startedAt??"")<(t.startedAt??"")?-1:1)}function Rt(e){const t=Ct(),s=ge(e),n=e.getBoundingClientRect();s.fillStyle=Z,s.fillRect(0,0,n.width,n.height);const r=Tt(t);if(t.length===0||r.length===0){s.fillStyle=W,s.font=ue,s.fillText("No scored runs to chart yet.",18,24);return}const i=46,d=n.width-150,c=30,u=n.height-24,p=Math.max(d-i,1),y=Math.max(u-c,1),o=Math.max(1,...r.flatMap(m=>m.values.filter(b=>b!=null))),$=m=>i+(t.length===1?p/2:p*m/(t.length-1)),R=m=>u-m/o*y;s.font=ae;for(let m=0;m<=4;m+=1){const b=o*m/4,T=R(b);s.strokeStyle=Te,s.beginPath(),s.moveTo(i,T),s.lineTo(d,T),s.stroke(),s.fillStyle=W,s.fillText(b.toFixed(2),8,T+4)}const v=(m,b)=>{const T=t.findIndex(x=>x.runId===m);if(T<0)return;const A=$(T);s.strokeStyle=W,s.setLineDash([3,3]),s.beginPath(),s.moveTo(A,c-4),s.lineTo(A,u),s.stroke(),s.setLineDash([]),s.fillStyle=J;const O=A>d-60;s.fillText(b,O?A-s.measureText(b).width-4:A+4,c+4)};v(a.evalBaselineRunId,"base"),v(a.evalSelectedRunId,"current"),s.fillStyle=W;const _=m=>m.length>8?`…${m.slice(-7)}`:m;t.length<=8?t.forEach((m,b)=>s.fillText(_(m.runId),$(b)-20,n.height-8)):(s.fillText(_(t[0].runId),i,n.height-8),s.fillText(_(t[t.length-1].runId),d-52,n.height-8),s.fillText(`${t.length} runs`,i+p/2-24,n.height-8)),r.forEach((m,b)=>{s.strokeStyle=m.color,s.lineWidth=2,s.beginPath();let T=!1;m.values.forEach((x,B)=>{if(x==null)return;const X=$(B),ye=R(x);T?s.lineTo(X,ye):s.moveTo(X,ye),T=!0}),s.stroke(),s.lineWidth=1,m.values.forEach((x,B)=>{x!=null&&(s.fillStyle=m.color,s.beginPath(),s.arc($(B),R(x),3.5,0,Math.PI*2),s.fill(),s.strokeStyle=Z,s.lineWidth=2,s.stroke(),s.lineWidth=1)});const A=m.values.reduce((x,B,X)=>B!=null?X:x,-1),O=c+8+b*16;s.fillStyle=m.color,s.fillRect(d+10,O-8,8,8),s.fillStyle=J,s.font=ae,s.fillText(`${m.label}${A>=0?` ${m.values[A].toFixed(2)}`:""}`.slice(0,22),d+22,O)});const I=m=>{let b=0,T=1/0;return t.forEach((A,O)=>{const x=Math.abs($(O)-m);x<T&&(T=x,b=O)}),b};e.onmousemove=m=>{const b=t[I(m.offsetX)];e.title=b?`${b.runId} · pass ${b.passRate==null?"-":`${(b.passRate*100).toFixed(0)}%`} · $${b.costUsd.toFixed(4)}`:""},e.onclick=m=>{const b=t[I(m.offsetX)];!b||b.runId===a.evalSelectedRunId||(a.evalSelectedRunId=b.runId,a.selectedEvalIdx=null,z().catch(T=>a.error=String(T)).finally(h))}}function Le(e,t,s,n,r,i,d=!1){e.strokeStyle=Te,e.fillStyle=W,e.font=ae,e.beginPath(),e.moveTo(t,s+12),e.lineTo(t+n,s+12),e.stroke();for(let c=0;c<=4;c+=1){const u=t+n*c/4;e.beginPath(),e.moveTo(u,s+7),e.lineTo(u,s+17),e.stroke();const p=r+(i-r)*c/4,y=d?`${Math.round(p*100)}%`:c===4?"now":`-${Math.round((i-p)/1e3)}s`;e.fillText(y,u+4,s+7)}}function Fe(e,t,s,n,r,i){e.fillStyle=i?Ue:s%56===0?je:Z,e.fillRect(t,s,n,r)}function qe(e,t,s,n,r,i){const d=Math.min(i,n/2,r/2);e.beginPath(),e.moveTo(t+d,s),e.arcTo(t+n,s,t+n,s+r,d),e.arcTo(t+n,s+r,t,s+r,d),e.arcTo(t,s+r,t,s,d),e.arcTo(t,s,t+n,s,d),e.closePath()}function Oe(e,t,s){const n=e.end-e.start,r=N(n*t,.03,1),i=e.start+n*N(s,0,1);e.start=N(i-r*s,0,1-r),e.end=e.start+r}window.addEventListener("keydown",e=>{if(!(e.target instanceof HTMLInputElement||e.target instanceof HTMLSelectElement)){if(e.key==="Escape"&&a.spanViewer){a.spanViewer=null,h();return}if(e.key==="Escape"&&a.attrPickerOpen){a.attrPickerOpen=!1,h();return}if(e.key==="1"&&(a.tab="traces"),e.key==="2"&&(a.tab="services"),e.key==="3"&&(a.tab="evals"),e.key==="4"&&(a.tab="timeline"),e.key==="Escape"&&a.tab==="detail"&&(a.tab=a.prevTab),e.key===" "&&(a.paused=!a.paused),e.key==="a"&&(V()||q())&&(a.attrPickerOpen=!0),e.key==="v"){const t=V()??q();t&&(a.spanViewer=t)}h()}});window.addEventListener("resize",h);async function xt(){Ae();try{const e=await S("initial_server");e.trim()&&(a.server=e.trim())}catch(e){console.warn("failed to load initial server",e)}try{await st()}catch(e){a.error=`failed to install live listeners: ${String(e)}`,h()}H()}xt();le=window.setInterval(()=>{a.connection==="connected"&&Promise.all([be(),z()]).catch(e=>{a.error=String(e),h()})},5e3);window.addEventListener("beforeunload",()=>{ne?.(),re?.(),le!=null&&window.clearInterval(le)});
