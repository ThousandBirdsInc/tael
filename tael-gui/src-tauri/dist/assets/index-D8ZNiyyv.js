(function(){const s=document.createElement("link").relList;if(s&&s.supports&&s.supports("modulepreload"))return;for(const n of document.querySelectorAll('link[rel="modulepreload"]'))r(n);new MutationObserver(n=>{for(const l of n)if(l.type==="childList")for(const u of l.addedNodes)u.tagName==="LINK"&&u.rel==="modulepreload"&&r(u)}).observe(document,{childList:!0,subtree:!0});function a(n){const l={};return n.integrity&&(l.integrity=n.integrity),n.referrerPolicy&&(l.referrerPolicy=n.referrerPolicy),n.crossOrigin==="use-credentials"?l.credentials="include":n.crossOrigin==="anonymous"?l.credentials="omit":l.credentials="same-origin",l}function r(n){if(n.ep)return;n.ep=!0;const l=a(n);fetch(n.href,l)}})();function fe(e,s=!1){return window.__TAURI_INTERNALS__.transformCallback(e,s)}async function $(e,s={},a){return window.__TAURI_INTERNALS__.invoke(e,s,a)}var z;(function(e){e.WINDOW_RESIZED="tauri://resize",e.WINDOW_MOVED="tauri://move",e.WINDOW_CLOSE_REQUESTED="tauri://close-requested",e.WINDOW_DESTROYED="tauri://destroyed",e.WINDOW_FOCUS="tauri://focus",e.WINDOW_BLUR="tauri://blur",e.WINDOW_SCALE_FACTOR_CHANGED="tauri://scale-change",e.WINDOW_THEME_CHANGED="tauri://theme-changed",e.WINDOW_CREATED="tauri://window-created",e.WINDOW_SUSPENDED="tauri://suspended",e.WINDOW_RESUMED="tauri://resumed",e.WEBVIEW_CREATED="tauri://webview-created",e.DRAG_ENTER="tauri://drag-enter",e.DRAG_OVER="tauri://drag-over",e.DRAG_DROP="tauri://drag-drop",e.DRAG_LEAVE="tauri://drag-leave"})(z||(z={}));async function ve(e,s){window.__TAURI_EVENT_PLUGIN_INTERNALS__.unregisterListener(e,s),await $("plugin:event|unlisten",{event:e,eventId:s})}async function X(e,s,a){var r;const n=(r=void 0)!==null&&r!==void 0?r:{kind:"Any"};return $("plugin:event|listen",{event:e,target:n,handler:fe(s)}).then(l=>async()=>ve(e,l))}const me=200,L=500,Y='12px "BerkeleyMono", ui-monospace, Menlo, Consolas, monospace',be='11px "BerkeleyMono", ui-monospace, Menlo, Consolas, monospace',q="#141414",he="#181818",$e="#2b2611",Se="#2a2a2a",J="#b5b5b1",K="#6f6f6c",Q="#ef4444",t={server:"http://127.0.0.1:7701",serviceFilter:"",statusFilter:"",lastWindow:"1h",textFilter:"",pinnedColumns:[],attrPickerOpen:!1,spanViewer:null,tab:"traces",prevTab:"traces",paused:!1,connection:"idle",error:null,streamId:crypto.randomUUID(),spans:[],selectedSpanIdx:null,services:[],selectedServiceIdx:null,liveTraceMap:new Map,liveTraces:[],selectedTraceIdx:null,timelineWindowMs:6e4,traceSpans:[],waterfallRows:[],selectedWaterfallIdx:null,currentTraceId:null,comments:[],commentDraft:"",evalRun:null,evalCases:[],selectedEvalIdx:null,evalFailuresOnly:!1,detailZoom:{start:0,end:1},liveZoom:{start:0,end:1}};let R=null,O=null,N=!1,F=null;const ee=document.querySelector("#app");if(!ee)throw new Error("missing #app");const p=ee;function o(){N||(N=!0,requestAnimationFrame(()=>{N=!1,ae()}))}function i(e){return String(e??"").replaceAll("&","&amp;").replaceAll("<","&lt;").replaceAll(">","&gt;").replaceAll('"',"&quot;")}function ye(e){const s=Date.parse(e);return Number.isFinite(s)?s:0}function P(e){return(Array.isArray(e)?e:Array.isArray(e?.spans)?e.spans:[]).map(a=>{const r=String(a.start_time??a.startTime??"-");return{traceId:String(a.trace_id??a.traceId??"-"),spanId:String(a.span_id??a.spanId??"-"),parentSpanId:a.parent_span_id??a.parentSpanId??null,service:String(a.service??"-"),operation:String(a.operation??"-"),durationMs:Number(a.duration_ms??a.durationMs??0),status:String(a.status??"-"),startTime:r,startTimeMs:ye(r),attributes:a.attributes&&typeof a.attributes=="object"?a.attributes:{},events:Array.isArray(a.events)?a.events:[]}})}function Ie(e){return(Array.isArray(e?.services)?e.services:[]).map(s=>({name:String(s.name??"-"),spanCount:Number(s.span_count??s.spanCount??0),traceCount:Number(s.trace_count??s.traceCount??0),avgDurationMs:Number(s.avg_duration_ms??s.avgDurationMs??0),errorRate:Number(s.error_rate??s.errorRate??0)}))}function V(e){return(Array.isArray(e?.comments)?e.comments:[]).map(s=>({author:String(s.author??"-"),body:String(s.body??""),createdAt:String(s.created_at??s.createdAt??"-"),spanId:s.span_id??s.spanId??null}))}function we(e){return e?{runId:String(e.run_id??e.runId??"-"),suiteId:String(e.suite_id??e.suiteId??"-"),status:String(e.status??"-"),caseCount:e.case_count??e.caseCount??null,observedCases:Number(e.observed_cases??e.observedCases??0),scoredCases:Number(e.scored_cases??e.scoredCases??0),passedCases:Number(e.passed_cases??e.passedCases??0),failedCases:Number(e.failed_cases??e.failedCases??0),costUsd:Number(e.cost_usd??e.costUsd??0),avgScores:e.avg_scores??e.avgScores??{}}:null}function ge(e){return(Array.isArray(e?.cases)?e.cases:[]).map(s=>({caseId:String(s.case_id??s.caseId??"-"),status:String(s.status??"-"),traceId:s.trace_id??s.traceId??null,durationMs:s.duration_ms??s.durationMs??null,costUsd:Number(s.cost_usd??s.costUsd??0),scores:s.scores??{},comments:V({comments:s.comments})}))}function w(e){const s=["#facc15","#62a9ff","#52d284","#b78cff","#f59e8c","#5ad1c9","#e0a3ff","#8fc4ff","#d4b483","#ff9ab0"];let a=0;for(const r of e)a=a*31+r.charCodeAt(0)>>>0;return s[a%s.length]}function A(e){return e>=500?"danger":e>=100?"warn":"ok"}function C(e){return e==="error"||e==="fail"?"danger":e==="ok"||e==="pass"?"ok":"muted"}function te(e){return(e.includes("T")?e.split("T")[1]:e).replace(/Z$/,"").slice(0,12)}function k(e,s=16){return e.length>s?`${e.slice(0,s)}...`:e}function Te(e,s){const a=e.attributes[s];return a==null?"":typeof a=="string"?a:JSON.stringify(a)}function Ee(){const e=M()??g(),s=new Set,a=[],r=n=>{if(n)for(const l of Object.keys(n.attributes))s.has(l)||(s.add(l),a.push(l))};r(e);for(const n of t.spans)r(n);for(const n of t.traceSpans)r(n);return a}function Ce(e){const s=t.pinnedColumns.indexOf(e);s>=0?t.pinnedColumns.splice(s,1):t.pinnedColumns.push(e)}function U(){const e=t.textFilter.trim().toLowerCase();return e?t.spans.filter(s=>s.service.toLowerCase().includes(e)||s.operation.toLowerCase().includes(e)||s.traceId.toLowerCase().includes(e)||s.status.toLowerCase().includes(e)):t.spans}function j(){const e=t.textFilter.trim().toLowerCase();return e?t.liveTraces.filter(s=>s.service.toLowerCase().includes(e)||s.operation.toLowerCase().includes(e)||s.traceId.toLowerCase().includes(e)||(s.hasError?"error":"ok").includes(e)):t.liveTraces}function D(){const e=t.textFilter.trim().toLowerCase();return t.evalCases.filter(s=>t.evalFailuresOnly&&s.status!=="fail"?!1:e?s.caseId.toLowerCase().includes(e)||s.status.toLowerCase().includes(e)||(s.traceId??"").toLowerCase().includes(e):!0)}function Me(e){if(e.length===0)return[];const s=Math.min(...e.map(d=>d.startTimeMs)),a=Math.max(...e.map(d=>d.startTimeMs+d.durationMs)),r=Math.max(a-s,1),n=new Map,l="__root__";e.forEach((d,m)=>{const b=d.parentSpanId??l,y=n.get(b)??[];y.push(m),n.set(b,y)});const u=[],f=[{parent:l,depth:0}];for(;f.length>0;){const d=f.pop(),m=n.get(d.parent)??[];for(const b of[...m].reverse()){const y=e[b];u.push({spanIdx:b,depth:d.depth,offsetPct:I((y.startTimeMs-s)/r,0,1),widthPct:I(y.durationMs/r,.005,1)}),f.push({parent:y.spanId,depth:d.depth+1})}}const v=new Set(u.map(d=>d.spanIdx));return e.forEach((d,m)=>{v.has(m)||u.push({spanIdx:m,depth:0,offsetPct:I((d.startTimeMs-s)/r,0,1),widthPct:I(d.durationMs/r,.005,1)})}),u}function I(e,s,a){return Math.max(s,Math.min(a,e))}function se(e){for(const s of e){const a=s.startTimeMs+s.durationMs,r=t.liveTraceMap.get(s.traceId);if(!r){t.liveTraceMap.set(s.traceId,{traceId:s.traceId,service:s.service,operation:s.operation,startTimeMs:s.startTimeMs,endTimeMs:a,durationMs:s.durationMs,spanCount:1,hasError:s.status==="error"});continue}r.startTimeMs=Math.min(r.startTimeMs,s.startTimeMs),r.endTimeMs=Math.max(r.endTimeMs,a),r.durationMs=r.endTimeMs-r.startTimeMs,r.spanCount+=1,r.hasError||=s.status==="error",s.parentSpanId||(r.service=s.service,r.operation=s.operation)}if(t.liveTraces=[...t.liveTraceMap.values()].sort((s,a)=>s.startTimeMs-a.startTimeMs),t.liveTraces.length>L){const s=t.liveTraces.slice(0,t.liveTraces.length-L);for(const a of s)t.liveTraceMap.delete(a.traceId);t.liveTraces=t.liveTraces.slice(-L)}}async function W(){const e=await $("query_traces",{server:t.server,request:{service:t.serviceFilter||null,status:t.statusFilter||null,last:t.lastWindow||"1h",limit:200,text:t.textFilter||null}});t.spans=P(e),se(t.spans)}async function Z(){t.services=Ie(await $("list_services",{server:t.server}))}async function B(){const e=await $("eval_runs",{server:t.server}),s=Array.isArray(e?.runs)?e.runs[0]:null,a=s?.run_id??s?.runId;if(!a){t.evalRun=null,t.evalCases=[];return}const r=await $("eval_status",{server:t.server,runId:a});t.evalRun=we(r?.run??r),t.evalCases=ge(await $("eval_cases",{server:t.server,runId:a}))}async function E(e){t.prevTab=t.tab==="detail"?t.prevTab:t.tab,t.tab="detail",t.currentTraceId=e,t.selectedWaterfallIdx=null,t.traceSpans=[],t.waterfallRows=[],t.comments=[],t.detailZoom={start:0,end:1},t.error=null,o();try{const[s,a]=await Promise.all([$("get_trace",{server:t.server,traceId:e}),$("get_comments",{server:t.server,traceId:e})]);t.traceSpans=P(s),t.waterfallRows=Me(t.traceSpans),t.selectedWaterfallIdx=t.waterfallRows.length>0?0:null,t.comments=V(a)}catch(s){t.error=String(s)}o()}async function _e(){if(!t.currentTraceId||!t.commentDraft.trim())return;const e=g();try{await $("add_comment",{server:t.server,request:{traceId:t.currentTraceId,body:t.commentDraft.trim(),author:"gui",spanId:e?.spanId??null}}),t.commentDraft="",t.comments=V(await $("get_comments",{server:t.server,traceId:t.currentTraceId}))}catch(s){t.error=String(s)}o()}async function x(){t.error=null,t.connection="checking",t.streamId=crypto.randomUUID(),o();try{await $("healthz",{server:t.server}),t.connection="loading",await Promise.all([W(),Z(),B()]),await xe(),t.connection="connected"}catch(e){t.connection="error",t.error=String(e)}o()}async function xe(){await $("start_live_stream",{server:t.server,service:t.serviceFilter||null,status:t.statusFilter||null,streamId:t.streamId})}async function Ae(){R?.(),O?.(),R=await X("tael://live-spans",e=>{if(!(e.payload.streamId!==t.streamId||t.paused))try{const s=P(JSON.parse(e.payload.data));if(s.length===0)return;se(s),t.spans=[...s,...t.spans].slice(0,me),t.error=null,o()}catch{}}),O=await X("tael://live-status",e=>{e.payload.streamId===t.streamId&&(t.connection=e.payload.status,e.payload.message&&(t.error=e.payload.message),o())})}function H(){const e=j();return t.selectedTraceIdx==null?null:e[t.selectedTraceIdx]??null}function M(){const e=U();return t.selectedSpanIdx==null?null:e[t.selectedSpanIdx]??null}function g(){if(t.selectedWaterfallIdx==null)return null;const e=t.waterfallRows[t.selectedWaterfallIdx];return e?t.traceSpans[e.spanIdx]:null}function _(e,s){return`<button class="tab ${t.tab===e?"active":""}" data-tab="${e}">${s}</button>`}function ae(){p.innerHTML=`
    <div class="shell">
      <header class="topbar">
        <div class="brand">
          <span class="brand-mark">◆</span>
          <span class="brand-name">tael</span>
          <span class="conn"><span class="conn-dot ${i(t.connection)}"></span>${i(t.connection)}</span>
        </div>
        <div class="conn-controls">
          <label class="field"><span>server</span><input id="server-input" class="server-input" value="${i(t.server)}" /></label>
          <label class="field"><span>service</span><input id="service-input" class="small-input" placeholder="all" value="${i(t.serviceFilter)}" /></label>
          <label class="field"><span>status</span>
            <select id="status-input" class="small-input">
              <option value="" ${t.statusFilter===""?"selected":""}>all</option>
              <option value="ok" ${t.statusFilter==="ok"?"selected":""}>ok</option>
              <option value="error" ${t.statusFilter==="error"?"selected":""}>error</option>
            </select>
          </label>
          <label class="field"><span>window</span><input id="last-input" class="tiny-input" value="${i(t.lastWindow)}" /></label>
          <button id="connect-btn" class="primary">Connect</button>
          <button id="refresh-btn" title="Refresh">Refresh</button>
          <button id="pause-btn" class="${t.paused?"active":""}" title="Pause live ingest">${t.paused?"Resume":"Pause"}</button>
        </div>
      </header>
      <nav class="subnav">
        <div class="tabs">
          ${_("traces","Traces")}
          ${_("services","Services")}
          ${_("evals","Evals")}
          ${_("timeline","Timeline")}
          ${t.tab==="detail"?_("detail","Trace"):""}
        </div>
        <div class="filter-box">
          <input id="filter-input" placeholder="filter…" value="${i(t.textFilter)}" />
          ${t.textFilter?'<button id="clear-filter-btn">Clear</button>':""}
        </div>
      </nav>
      ${t.error?`<div class="error-bar">${i(t.error)}</div>`:'<div class="error-bar is-hidden"></div>'}
      <main class="workspace">${ke()}</main>
      ${t.attrPickerOpen?We():""}
      ${t.spanViewer?qe(t.spanViewer):""}
    </div>
  `,Pe(),Ve()}function ke(){return t.tab==="services"?Ne():t.tab==="evals"?Re():t.tab==="timeline"?Fe():t.tab==="detail"?De():Le()}function Le(){const e=U(),s=M(),a=t.pinnedColumns.map(r=>`<th>${i(r)}</th>`).join("");return`
    <section class="split vertical">
      <div class="pane table-pane">
        <div class="pane-title">
          <span>Traces</span>
          <span>${e.length}/${t.spans.length}</span>
        </div>
        <div class="table-wrap">
          <table>
            <thead><tr><th>Time</th><th>Service</th><th>Operation</th><th>Duration</th><th>Status</th><th>Trace ID</th>${a}</tr></thead>
            <tbody>
              ${e.map((r,n)=>`
                <tr class="${t.selectedSpanIdx===n?"selected":""}" data-span-idx="${n}">
                  <td class="muted">${i(te(r.startTime))}</td>
                  <td style="color:${w(r.service)}">${i(r.service)}</td>
                  <td>${i(r.operation)}</td>
                  <td class="${A(r.durationMs)}">${r.durationMs.toFixed(0)}ms</td>
                  <td class="${C(r.status)}">${i(r.status)}</td>
                  <td class="mono muted">${i(k(r.traceId))}</td>
                  ${t.pinnedColumns.map(l=>{const u=Te(r,l);return`<td class="${u?"attr-cell":"muted"}">${i(u||"-")}</td>`}).join("")}
                </tr>
              `).join("")}
            </tbody>
          </table>
        </div>
      </div>
      <aside class="pane detail-pane">${s?re(s):'<div class="empty">No span selected.</div>'}</aside>
    </section>
  `}function re(e){return`
    <div class="pane-title">
      <span>Span</span>
      <div class="button-row">
        <button id="pin-columns-btn">Columns</button>
        <button id="view-span-btn">View</button>
        <button id="open-selected-trace-btn">Open Trace</button>
      </div>
    </div>
    <dl class="properties">
      <dt>trace_id</dt><dd class="mono">${i(e.traceId)}</dd>
      <dt>span_id</dt><dd class="mono">${i(e.spanId)}</dd>
      <dt>parent</dt><dd class="mono">${i(e.parentSpanId??"none")}</dd>
      <dt>service</dt><dd style="color:${w(e.service)}">${i(e.service)}</dd>
      <dt>operation</dt><dd>${i(e.operation)}</dd>
      <dt>status</dt><dd class="${C(e.status)}">${i(e.status)}</dd>
      <dt>duration</dt><dd class="${A(e.durationMs)}">${e.durationMs.toFixed(2)}ms</dd>
      <dt>start</dt><dd>${i(e.startTime)}</dd>
    </dl>
    <pre class="json-view">${i(JSON.stringify({attributes:e.attributes,events:e.events},null,2))}</pre>
  `}function Ne(){return`
    <section class="pane table-pane full">
      <div class="pane-title"><span>Services</span><span>${t.services.length}</span></div>
      <div class="table-wrap">
        <table>
          <thead><tr><th>Service</th><th>Spans</th><th>Traces</th><th>Avg Duration</th><th>Error Rate</th></tr></thead>
          <tbody>
            ${t.services.map((e,s)=>`
              <tr class="${t.selectedServiceIdx===s?"selected":""}" data-service-idx="${s}">
                <td style="color:${w(e.name)}">${i(e.name)}</td>
                <td>${e.spanCount}</td>
                <td>${e.traceCount}</td>
                <td class="${A(e.avgDurationMs)}">${e.avgDurationMs.toFixed(1)}ms</td>
                <td class="${e.errorRate>.05?"danger":e.errorRate>0?"warn":"ok"}">${(e.errorRate*100).toFixed(1)}%</td>
              </tr>
            `).join("")}
          </tbody>
        </table>
      </div>
    </section>
  `}function Re(){const e=t.evalRun,s=D(),a=t.selectedEvalIdx==null?null:s[t.selectedEvalIdx];if(!e)return'<section class="pane full"><div class="empty">No eval runs found.</div></section>';const r=typeof e.avgScores.correctness=="number"?e.avgScores.correctness.toFixed(3):"-";return`
    <section class="split vertical eval-layout">
      <div class="pane run-strip">
        <div class="run-stat grow">
          <span class="run-stat-label">Suite</span>
          <span class="run-stat-value">${i(e.suiteId)}</span>
          <span class="run-stat-sub mono">${i(e.runId)}</span>
        </div>
        <div class="run-stat">
          <span class="run-stat-label">Status</span>
          <span class="run-stat-value ${C(e.status)}">${i(e.status)}</span>
        </div>
        <div class="run-stat">
          <span class="run-stat-label">Cases</span>
          <span class="run-stat-value">${e.observedCases}<span class="run-stat-sub"> / ${e.caseCount??"?"}</span></span>
        </div>
        <div class="run-stat">
          <span class="run-stat-label">Pass</span>
          <span class="run-stat-value ok">${e.passedCases}</span>
        </div>
        <div class="run-stat">
          <span class="run-stat-label">Fail</span>
          <span class="run-stat-value ${e.failedCases>0?"danger":""}">${e.failedCases}</span>
        </div>
        <div class="run-stat">
          <span class="run-stat-label">Avg score</span>
          <span class="run-stat-value">${r}</span>
        </div>
        <div class="run-stat">
          <span class="run-stat-label">Cost</span>
          <span class="run-stat-value">$${e.costUsd.toFixed(4)}</span>
        </div>
        <button id="failures-only-btn" class="spacer ${t.evalFailuresOnly?"active":""}">Failures</button>
      </div>
      <div class="pane table-pane">
        <div class="pane-title"><span>Cases</span><span>${s.length}</span></div>
        <div class="table-wrap">
          <table>
            <thead><tr><th>Status</th><th>Case</th><th>Score</th><th>Cost</th><th>Duration</th><th>Trace</th></tr></thead>
            <tbody>
              ${s.map((n,l)=>{const u=typeof n.scores.correctness=="number"?n.scores.correctness.toFixed(3):Object.values(n.scores).find(f=>typeof f=="number")?.toString()??"-";return`
                  <tr class="${t.selectedEvalIdx===l?"selected":""}" data-eval-idx="${l}">
                    <td class="${C(n.status)}">${i(n.status.toUpperCase())}</td>
                    <td>${i(n.caseId)}</td>
                    <td>${i(u)}</td>
                    <td>${n.costUsd.toFixed(4)}</td>
                    <td>${n.durationMs==null?"-":`${n.durationMs.toFixed(0)}ms`}</td>
                    <td class="mono muted">${i(n.traceId?k(n.traceId,12):"-")}</td>
                  </tr>
                `}).join("")}
            </tbody>
          </table>
        </div>
      </div>
      <aside class="pane detail-pane">${a?Oe(a):'<div class="empty">No case selected.</div>'}</aside>
    </section>
  `}function Oe(e){return`
    <div class="pane-title">
      <span>${i(e.caseId)}</span>
      ${e.traceId?'<button id="open-eval-trace-btn">Open Trace</button>':""}
    </div>
    <dl class="properties">
      <dt>status</dt><dd class="${C(e.status)}">${i(e.status)}</dd>
      <dt>trace</dt><dd class="mono">${i(e.traceId??"-")}</dd>
      <dt>duration</dt><dd>${e.durationMs==null?"-":`${e.durationMs.toFixed(1)}ms`}</dd>
      <dt>cost</dt><dd>$${e.costUsd.toFixed(4)}</dd>
    </dl>
    <pre class="json-view">${i(JSON.stringify(e.scores,null,2))}</pre>
    ${e.comments.length?`<div class="comment-list">${e.comments.map(ne).join("")}</div>`:""}
  `}function Fe(){const e=H();return`
    <section class="split vertical">
      <div class="pane timeline-pane">
        <div class="pane-title">
          <span>Live Timeline</span>
          <span>${j().length}/${t.liveTraces.length} traces</span>
        </div>
        <canvas id="timeline-canvas" class="timeline-canvas"></canvas>
      </div>
      <aside class="pane detail-pane">
        ${e?`
          <div class="pane-title"><span>Trace</span><button id="open-selected-live-trace-btn">Open Trace</button></div>
          <dl class="properties">
            <dt>trace_id</dt><dd class="mono">${i(e.traceId)}</dd>
            <dt>service</dt><dd style="color:${w(e.service)}">${i(e.service)}</dd>
            <dt>operation</dt><dd>${i(e.operation)}</dd>
            <dt>status</dt><dd class="${e.hasError?"danger":"ok"}">${e.hasError?"error":"ok"}</dd>
            <dt>duration</dt><dd class="${A(e.durationMs)}">${e.durationMs.toFixed(2)}ms</dd>
            <dt>spans</dt><dd>${e.spanCount}</dd>
          </dl>
        `:'<div class="empty">No trace selected.</div>'}
      </aside>
    </section>
  `}function De(){const e=g();return`
    <section class="detail-grid">
      <div class="pane waterfall-pane">
        <div class="pane-title">
          <span>${i(t.currentTraceId?`Trace ${k(t.currentTraceId)}`:"Trace")}</span>
          <button id="back-btn">Back</button>
        </div>
        <canvas id="waterfall-canvas" class="waterfall-canvas"></canvas>
      </div>
      <aside class="pane span-side">
        ${e?re(e):'<div class="empty">No span selected.</div>'}
      </aside>
      <section class="pane comments-pane">
        <div class="pane-title"><span>Comments</span><span>${t.comments.length}</span></div>
        <div class="comment-list">${t.comments.map(ne).join("")||'<div class="empty compact">No comments.</div>'}</div>
        <div class="comment-form">
          <input id="comment-input" value="${i(t.commentDraft)}" />
          <button id="submit-comment-btn">Add</button>
        </div>
      </section>
    </section>
  `}function ne(e){const s=te(e.createdAt).slice(0,8);return`
    <div class="comment">
      <span class="muted">${i(s)}</span>
      <strong>${i(e.author)}</strong>
      ${e.spanId?`<span class="mono muted">${i(k(e.spanId,8))}</span>`:""}
      <p>${i(e.body)}</p>
    </div>
  `}function We(){const e=Ee();return`
    <div class="overlay">
      <section class="modal attr-modal">
        <div class="modal-title">
          <span>Pin Attribute Columns</span>
          <button id="close-attr-picker-btn">Close</button>
        </div>
        <div class="modal-body">
          ${e.length?e.map(s=>`
                <label class="check-row">
                  <input type="checkbox" data-attr-key="${i(s)}" ${t.pinnedColumns.includes(s)?"checked":""} />
                  <span class="mono">${i(s)}</span>
                </label>
              `).join(""):'<div class="empty compact">No attributes found.</div>'}
        </div>
      </section>
    </div>
  `}function qe(e){return`
    <div class="overlay">
      <section class="modal span-modal">
        <div class="modal-title">
          <span>${i(e.service)} / ${i(e.operation)}</span>
          <button id="close-span-viewer-btn">Close</button>
        </div>
        <div class="modal-body split-modal">
          <dl class="properties modal-properties">
            <dt>trace_id</dt><dd class="mono">${i(e.traceId)}</dd>
            <dt>span_id</dt><dd class="mono">${i(e.spanId)}</dd>
            <dt>parent</dt><dd class="mono">${i(e.parentSpanId??"none")}</dd>
            <dt>service</dt><dd style="color:${w(e.service)}">${i(e.service)}</dd>
            <dt>operation</dt><dd>${i(e.operation)}</dd>
            <dt>status</dt><dd class="${C(e.status)}">${i(e.status)}</dd>
            <dt>duration</dt><dd class="${A(e.durationMs)}">${e.durationMs.toFixed(2)}ms</dd>
            <dt>start</dt><dd>${i(e.startTime)}</dd>
          </dl>
          <pre class="json-view modal-json">${i(JSON.stringify({attributes:e.attributes,events:e.events},null,2))}</pre>
        </div>
      </section>
    </div>
  `}function Pe(){p.querySelector("#server-input")?.addEventListener("change",e=>{t.server=e.currentTarget.value.trim()}),p.querySelector("#service-input")?.addEventListener("change",e=>{t.serviceFilter=e.currentTarget.value.trim(),x()}),p.querySelector("#status-input")?.addEventListener("change",e=>{t.statusFilter=e.currentTarget.value,x()}),p.querySelector("#last-input")?.addEventListener("change",e=>{t.lastWindow=e.currentTarget.value.trim()||"1h",W().catch(s=>t.error=String(s)).finally(o)}),p.querySelector("#filter-input")?.addEventListener("input",e=>{t.textFilter=e.currentTarget.value,t.selectedSpanIdx=null,t.selectedTraceIdx=null,t.selectedEvalIdx=null,o()}),p.querySelector("#clear-filter-btn")?.addEventListener("click",()=>{t.textFilter="",o()}),p.querySelector("#connect-btn")?.addEventListener("click",x),p.querySelector("#refresh-btn")?.addEventListener("click",()=>{Promise.all([W(),Z(),B()]).catch(e=>t.error=String(e)).finally(o)}),p.querySelector("#pause-btn")?.addEventListener("click",()=>{t.paused=!t.paused,o()}),p.querySelectorAll("[data-tab]").forEach(e=>{e.addEventListener("click",()=>{t.tab=e.dataset.tab,o()})}),p.querySelectorAll("[data-span-idx]").forEach(e=>{e.addEventListener("click",()=>{t.selectedSpanIdx=Number(e.dataset.spanIdx),o()}),e.addEventListener("dblclick",()=>{const s=U()[Number(e.dataset.spanIdx)];s&&E(s.traceId)})}),p.querySelector("#open-selected-trace-btn")?.addEventListener("click",()=>{const e=M()??g();e&&E(e.traceId)}),p.querySelector("#pin-columns-btn")?.addEventListener("click",()=>{t.attrPickerOpen=!0,o()}),p.querySelector("#view-span-btn")?.addEventListener("click",()=>{const e=M()??g();e&&(t.spanViewer=e,o())}),p.querySelector("#close-attr-picker-btn")?.addEventListener("click",()=>{t.attrPickerOpen=!1,o()}),p.querySelectorAll("[data-attr-key]").forEach(e=>{e.addEventListener("change",()=>{const s=e.dataset.attrKey;s&&Ce(s),o()})}),p.querySelector("#close-span-viewer-btn")?.addEventListener("click",()=>{t.spanViewer=null,o()}),p.querySelectorAll("[data-service-idx]").forEach(e=>{e.addEventListener("click",()=>{const s=t.services[Number(e.dataset.serviceIdx)];s&&(t.selectedServiceIdx=Number(e.dataset.serviceIdx),t.serviceFilter=s.name,t.tab="traces",x())})}),p.querySelector("#failures-only-btn")?.addEventListener("click",()=>{t.evalFailuresOnly=!t.evalFailuresOnly,t.selectedEvalIdx=null,o()}),p.querySelectorAll("[data-eval-idx]").forEach(e=>{e.addEventListener("click",()=>{t.selectedEvalIdx=Number(e.dataset.evalIdx),o()}),e.addEventListener("dblclick",()=>{const s=D()[Number(e.dataset.evalIdx)];s?.traceId&&E(s.traceId)})}),p.querySelector("#open-eval-trace-btn")?.addEventListener("click",()=>{const e=t.selectedEvalIdx==null?null:D()[t.selectedEvalIdx];e?.traceId&&E(e.traceId)}),p.querySelector("#open-selected-live-trace-btn")?.addEventListener("click",()=>{const e=H();e&&E(e.traceId)}),p.querySelector("#back-btn")?.addEventListener("click",()=>{t.tab=t.prevTab,o()}),p.querySelector("#comment-input")?.addEventListener("input",e=>{t.commentDraft=e.currentTarget.value}),p.querySelector("#submit-comment-btn")?.addEventListener("click",_e)}function Ve(){const e=p.querySelector("#timeline-canvas");e&&Ue(e);const s=p.querySelector("#waterfall-canvas");s&&je(s)}function ie(e){const s=e.getBoundingClientRect(),a=window.devicePixelRatio||1;e.width=Math.max(1,Math.floor(s.width*a)),e.height=Math.max(1,Math.floor(s.height*a));const r=e.getContext("2d");if(!r)throw new Error("2d canvas unavailable");return r.scale(a,a),r.clearRect(0,0,s.width,s.height),r}function Ue(e){const s=j(),a=ie(e),r=e.getBoundingClientRect(),n=260,l=26,u=34,f=Math.max(r.width-n-96,1),d=s.reduce((c,S)=>Math.max(c,S.endTimeMs),0)-t.timelineWindowMs,m=d+t.timelineWindowMs*t.liveZoom.start,b=d+t.timelineWindowMs*t.liveZoom.end,y=Math.max(b-m,1);a.fillStyle=q,a.fillRect(0,0,r.width,r.height),le(a,n,12,f,m,b);const T=s.filter(c=>c.endTimeMs>=m&&c.startTimeMs<=b);T.forEach((c,S)=>{const h=u+S*l;if(h>r.height-l)return;const ue=s.indexOf(c)===t.selectedTraceIdx;oe(a,0,h-3,r.width,l,ue),a.fillStyle=w(c.service),a.font=Y,a.fillText(`${c.service} ${c.operation}`.slice(0,34),18,h+13);const G=n+I((c.startTimeMs-m)/y,0,1)*f,pe=Math.max(2,c.durationMs/y*f);a.fillStyle=c.hasError?Q:w(c.service),ce(a,G,h,Math.min(pe,n+f-G),14,3),a.fill(),a.fillStyle=J,a.fillText(`${c.durationMs.toFixed(0)}ms`,n+f+14,h+12),a.fillStyle=K,a.fillText(String(c.spanCount),n+f+68,h+12)}),e.onmousemove=c=>{const S=Math.floor((c.offsetY-u)/l),h=T[S];e.title=h?`${h.service} ${h.operation} ${h.durationMs.toFixed(1)}ms`:""},e.onclick=c=>{const S=Math.floor((c.offsetY-u)/l),h=T[S];h&&(t.selectedTraceIdx=s.indexOf(h),o())},e.ondblclick=()=>{const c=H();c&&E(c.traceId)},e.onwheel=c=>{c.preventDefault();const S=c.deltaY>0?1.18:.84;de(t.liveZoom,S,c.offsetX/r.width),o()}}function je(e){const s=ie(e),a=e.getBoundingClientRect(),r=t.waterfallRows,n=300,l=28,u=36,f=Math.max(a.width-n-92,1);s.fillStyle=q,s.fillRect(0,0,a.width,a.height),le(s,n,12,f,t.detailZoom.start,t.detailZoom.end,!0),r.forEach((v,d)=>{const m=t.traceSpans[v.spanIdx],b=u+d*l;if(b>a.height-l)return;const y=t.selectedWaterfallIdx===d;oe(s,0,b-4,a.width,l,y),s.font=Y,s.fillStyle=w(m.service),s.fillText(`${" ".repeat(v.depth*2)}${m.service} ${m.operation}`.slice(0,42),18,b+13);const T=t.detailZoom.end-t.detailZoom.start,c=n+(v.offsetPct-t.detailZoom.start)/T*f,S=Math.max(2,v.widthPct/T*f);c+S<n||c>n+f||(s.fillStyle=m.status==="error"?Q:w(m.service),ce(s,I(c,n,n+f),b,Math.min(S,n+f-c),15,3),s.fill(),s.fillStyle=J,s.fillText(`${m.durationMs.toFixed(0)}ms`,n+f+14,b+12))}),e.onclick=v=>{const d=Math.floor((v.offsetY-u)/l);r[d]&&(t.selectedWaterfallIdx=d,o())},e.ondblclick=()=>{const v=g();v&&(t.selectedSpanIdx=t.spans.findIndex(d=>d.spanId===v.spanId))},e.onwheel=v=>{v.preventDefault(),de(t.detailZoom,v.deltaY>0?1.18:.84,v.offsetX/a.width),o()}}function le(e,s,a,r,n,l,u=!1){e.strokeStyle=Se,e.fillStyle=K,e.font=be,e.beginPath(),e.moveTo(s,a+12),e.lineTo(s+r,a+12),e.stroke();for(let f=0;f<=4;f+=1){const v=s+r*f/4;e.beginPath(),e.moveTo(v,a+7),e.lineTo(v,a+17),e.stroke();const d=n+(l-n)*f/4,m=u?`${Math.round(d*100)}%`:f===4?"now":`-${Math.round((l-d)/1e3)}s`;e.fillText(m,v+4,a+7)}}function oe(e,s,a,r,n,l){e.fillStyle=l?$e:a%56===0?he:q,e.fillRect(s,a,r,n)}function ce(e,s,a,r,n,l){const u=Math.min(l,r/2,n/2);e.beginPath(),e.moveTo(s+u,a),e.arcTo(s+r,a,s+r,a+n,u),e.arcTo(s+r,a+n,s,a+n,u),e.arcTo(s,a+n,s,a,u),e.arcTo(s,a,s+r,a,u),e.closePath()}function de(e,s,a){const r=e.end-e.start,n=I(r*s,.03,1),l=e.start+r*I(a,0,1);e.start=I(l-n*a,0,1-n),e.end=e.start+n}window.addEventListener("keydown",e=>{if(!(e.target instanceof HTMLInputElement||e.target instanceof HTMLSelectElement)){if(e.key==="Escape"&&t.spanViewer){t.spanViewer=null,o();return}if(e.key==="Escape"&&t.attrPickerOpen){t.attrPickerOpen=!1,o();return}if(e.key==="1"&&(t.tab="traces"),e.key==="2"&&(t.tab="services"),e.key==="3"&&(t.tab="evals"),e.key==="4"&&(t.tab="timeline"),e.key==="Escape"&&t.tab==="detail"&&(t.tab=t.prevTab),e.key===" "&&(t.paused=!t.paused),e.key==="a"&&(M()||g())&&(t.attrPickerOpen=!0),e.key==="v"){const s=M()??g();s&&(t.spanViewer=s)}o()}});window.addEventListener("resize",o);async function Ze(){ae();try{const e=await $("initial_server");e.trim()&&(t.server=e.trim())}catch(e){console.warn("failed to load initial server",e)}try{await Ae()}catch(e){t.error=`failed to install live listeners: ${String(e)}`,o()}x()}Ze();F=window.setInterval(()=>{t.connection==="connected"&&Promise.all([Z(),B()]).catch(e=>{t.error=String(e),o()})},5e3);window.addEventListener("beforeunload",()=>{R?.(),O?.(),F!=null&&window.clearInterval(F)});
