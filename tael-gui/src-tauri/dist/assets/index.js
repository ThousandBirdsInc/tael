(function(){const e=document.createElement("link").relList;if(e&&e.supports&&e.supports("modulepreload"))return;for(const a of document.querySelectorAll('link[rel="modulepreload"]'))n(a);new MutationObserver(a=>{for(const o of a)if(o.type==="childList")for(const d of o.addedNodes)d.tagName==="LINK"&&d.rel==="modulepreload"&&n(d)}).observe(document,{childList:!0,subtree:!0});function r(a){const o={};return a.integrity&&(o.integrity=a.integrity),a.referrerPolicy&&(o.referrerPolicy=a.referrerPolicy),a.crossOrigin==="use-credentials"?o.credentials="include":a.crossOrigin==="anonymous"?o.credentials="omit":o.credentials="same-origin",o}function n(a){if(a.ep)return;a.ep=!0;const o=r(a);fetch(a.href,o)}})();function _t(t,e=!1){return window.__TAURI_INTERNALS__.transformCallback(t,e)}async function h(t,e={},r){return window.__TAURI_INTERNALS__.invoke(t,e,r)}var at;(function(t){t.WINDOW_RESIZED="tauri://resize",t.WINDOW_MOVED="tauri://move",t.WINDOW_CLOSE_REQUESTED="tauri://close-requested",t.WINDOW_DESTROYED="tauri://destroyed",t.WINDOW_FOCUS="tauri://focus",t.WINDOW_BLUR="tauri://blur",t.WINDOW_SCALE_FACTOR_CHANGED="tauri://scale-change",t.WINDOW_THEME_CHANGED="tauri://theme-changed",t.WINDOW_CREATED="tauri://window-created",t.WINDOW_SUSPENDED="tauri://suspended",t.WINDOW_RESUMED="tauri://resumed",t.WEBVIEW_CREATED="tauri://webview-created",t.DRAG_ENTER="tauri://drag-enter",t.DRAG_OVER="tauri://drag-over",t.DRAG_DROP="tauri://drag-drop",t.DRAG_LEAVE="tauri://drag-leave"})(at||(at={}));async function Tt(t,e){window.__TAURI_EVENT_PLUGIN_INTERNALS__.unregisterListener(t,e),await h("plugin:event|unlisten",{event:t,eventId:e})}async function nt(t,e,r){var n;const a=(n=void 0)!==null&&n!==void 0?n:{kind:"Any"};return h("plugin:event|listen",{event:t,target:a,handler:_t(e)}).then(o=>async()=>Tt(t,o))}const Ct=["health","topology","automation","clusters","review","sql"];function V(t){return Ct.includes(t)}function A(){return{loaded:!1,error:null,data:null}}const Et=200,W=500,it='12px "BerkeleyMono", ui-monospace, Menlo, Consolas, monospace',Mt='11px "BerkeleyMono", ui-monospace, Menlo, Consolas, monospace',Q="#141414",kt="#181818",xt="#2b2611",At="#2a2a2a",ot="#b5b5b1",lt="#6f6f6c",dt="#ef4444";function ct(){return typeof crypto<"u"&&"randomUUID"in crypto?crypto.randomUUID():`${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`}const s={server:"http://127.0.0.1:7701",serviceFilter:"",statusFilter:"",lastWindow:"1h",textFilter:"",pinnedColumns:[],attrPickerOpen:!1,spanViewer:null,tab:"traces",prevTab:"traces",paused:!1,connection:"idle",error:null,streamId:ct(),spans:[],selectedSpanIdx:null,services:[],selectedServiceIdx:null,liveTraceMap:new Map,liveTraces:[],selectedTraceIdx:null,timelineWindowMs:6e4,traceSpans:[],waterfallRows:[],selectedWaterfallIdx:null,currentTraceId:null,comments:[],commentDraft:"",evalRun:null,evalCases:[],selectedEvalIdx:null,evalFailuresOnly:!1,detailZoom:{start:0,end:1},liveZoom:{start:0,end:1},panels:{health:A(),topology:A(),automation:A(),clusters:A(),review:A(),sql:A()},sqlQuery:"SELECT service, count(*) AS spans FROM spans GROUP BY service ORDER BY spans DESC",suites:[]};let j=null,U=null,P=!1,B=null;const ut=document.querySelector("#app");if(!ut)throw new Error("missing #app");const p=ut;function u(){P||(P=!0,requestAnimationFrame(()=>{P=!1,vt()}))}function i(t){return String(t??"").replaceAll("&","&amp;").replaceAll("<","&lt;").replaceAll(">","&gt;").replaceAll('"',"&quot;")}function Nt(t){const e=Date.parse(t);return Number.isFinite(e)?e:0}function G(t){return(Array.isArray(t)?t:Array.isArray(t?.spans)?t.spans:[]).map(r=>{const n=String(r.start_time??r.startTime??"-");return{traceId:String(r.trace_id??r.traceId??"-"),spanId:String(r.span_id??r.spanId??"-"),parentSpanId:r.parent_span_id??r.parentSpanId??null,service:String(r.service??"-"),operation:String(r.operation??"-"),durationMs:Number(r.duration_ms??r.durationMs??0),status:String(r.status??"-"),startTime:n,startTimeMs:Nt(n),attributes:r.attributes&&typeof r.attributes=="object"?r.attributes:{},events:Array.isArray(r.events)?r.events:[]}})}function Rt(t){return(Array.isArray(t?.services)?t.services:[]).map(e=>({name:String(e.name??"-"),spanCount:Number(e.span_count??e.spanCount??0),traceCount:Number(e.trace_count??e.traceCount??0),avgDurationMs:Number(e.avg_duration_ms??e.avgDurationMs??0),errorRate:Number(e.error_rate??e.errorRate??0)}))}function Y(t){return(Array.isArray(t?.comments)?t.comments:[]).map(e=>({author:String(e.author??"-"),body:String(e.body??""),createdAt:String(e.created_at??e.createdAt??"-"),spanId:e.span_id??e.spanId??null}))}function Lt(t){return t?{runId:String(t.run_id??t.runId??"-"),suiteId:String(t.suite_id??t.suiteId??"-"),status:String(t.status??"-"),caseCount:t.case_count??t.caseCount??null,observedCases:Number(t.observed_cases??t.observedCases??0),scoredCases:Number(t.scored_cases??t.scoredCases??0),passedCases:Number(t.passed_cases??t.passedCases??0),failedCases:Number(t.failed_cases??t.failedCases??0),costUsd:Number(t.cost_usd??t.costUsd??0),avgScores:t.avg_scores??t.avgScores??{}}:null}function qt(t){return(Array.isArray(t?.cases)?t.cases:[]).map(e=>({caseId:String(e.case_id??e.caseId??"-"),status:String(e.status??"-"),traceId:e.trace_id??e.traceId??null,durationMs:e.duration_ms??e.durationMs??null,costUsd:Number(e.cost_usd??e.costUsd??0),scores:e.scores??{},comments:Y({comments:e.comments})}))}function C(t){const e=["#facc15","#62a9ff","#52d284","#b78cff","#f59e8c","#5ad1c9","#e0a3ff","#8fc4ff","#d4b483","#ff9ab0"];let r=0;for(const n of t)r=r*31+n.charCodeAt(0)>>>0;return e[r%e.length]}function F(t){return t>=500?"danger":t>=100?"warn":"ok"}function N(t){return t==="error"||t==="fail"?"danger":t==="ok"||t==="pass"?"ok":"muted"}function J(t){return(t.includes("T")?t.split("T")[1]:t).replace(/Z$/,"").slice(0,12)}function O(t,e=16){return t.length>e?`${t.slice(0,e)}...`:t}function Ft(t,e){const r=t.attributes[e];return r==null?"":typeof r=="string"?r:JSON.stringify(r)}function Ot(){const t=R()??k(),e=new Set,r=[],n=a=>{if(a)for(const o of Object.keys(a.attributes))e.has(o)||(e.add(o),r.push(o))};n(t);for(const a of s.spans)n(a);for(const a of s.traceSpans)n(a);return r}function Dt(t){const e=s.pinnedColumns.indexOf(t);e>=0?s.pinnedColumns.splice(e,1):s.pinnedColumns.push(t)}function X(){const t=s.textFilter.trim().toLowerCase();return t?s.spans.filter(e=>e.service.toLowerCase().includes(t)||e.operation.toLowerCase().includes(t)||e.traceId.toLowerCase().includes(t)||e.status.toLowerCase().includes(t)):s.spans}function K(){const t=s.textFilter.trim().toLowerCase();return t?s.liveTraces.filter(e=>e.service.toLowerCase().includes(t)||e.operation.toLowerCase().includes(t)||e.traceId.toLowerCase().includes(t)||(e.hasError?"error":"ok").includes(t)):s.liveTraces}function H(){const t=s.textFilter.trim().toLowerCase();return s.evalCases.filter(e=>s.evalFailuresOnly&&e.status!=="fail"?!1:t?e.caseId.toLowerCase().includes(t)||e.status.toLowerCase().includes(t)||(e.traceId??"").toLowerCase().includes(t):!0)}function Wt(t){if(t.length===0)return[];const e=Math.min(...t.map(f=>f.startTimeMs)),r=Math.max(...t.map(f=>f.startTimeMs+f.durationMs)),n=Math.max(r-e,1),a=new Map,o="__root__";t.forEach((f,b)=>{const y=f.parentSpanId??o,S=a.get(y)??[];S.push(b),a.set(y,S)});const d=[],l=[{parent:o,depth:0}];for(;l.length>0;){const f=l.pop(),b=a.get(f.parent)??[];for(const y of[...b].reverse()){const S=t[y];d.push({spanIdx:y,depth:f.depth,offsetPct:T((S.startTimeMs-e)/n,0,1),widthPct:T(S.durationMs/n,.005,1)}),l.push({parent:S.spanId,depth:f.depth+1})}}const c=new Set(d.map(f=>f.spanIdx));return t.forEach((f,b)=>{c.has(b)||d.push({spanIdx:b,depth:0,offsetPct:T((f.startTimeMs-e)/n,0,1),widthPct:T(f.durationMs/n,.005,1)})}),d}function T(t,e,r){return Math.max(e,Math.min(r,t))}function pt(t){for(const e of t){const r=e.startTimeMs+e.durationMs,n=s.liveTraceMap.get(e.traceId);if(!n){s.liveTraceMap.set(e.traceId,{traceId:e.traceId,service:e.service,operation:e.operation,startTimeMs:e.startTimeMs,endTimeMs:r,durationMs:e.durationMs,spanCount:1,hasError:e.status==="error"});continue}n.startTimeMs=Math.min(n.startTimeMs,e.startTimeMs),n.endTimeMs=Math.max(n.endTimeMs,r),n.durationMs=n.endTimeMs-n.startTimeMs,n.spanCount+=1,n.hasError||=e.status==="error",e.parentSpanId||(n.service=e.service,n.operation=e.operation)}if(s.liveTraces=[...s.liveTraceMap.values()].sort((e,r)=>e.startTimeMs-r.startTimeMs),s.liveTraces.length>W){const e=s.liveTraces.slice(0,s.liveTraces.length-W);for(const r of e)s.liveTraceMap.delete(r.traceId);s.liveTraces=s.liveTraces.slice(-W)}}async function Z(){const t=await h("query_traces",{server:s.server,request:{service:s.serviceFilter||null,status:s.statusFilter||null,last:s.lastWindow||"1h",limit:200,text:s.textFilter||null}});s.spans=G(t),pt(s.spans)}async function tt(){s.services=Rt(await h("list_services",{server:s.server}))}async function et(){const t=await h("eval_runs",{server:s.server}),e=Array.isArray(t?.runs)?t.runs[0]:null,r=e?.run_id??e?.runId;if(!r){s.evalRun=null,s.evalCases=[];return}const n=await h("eval_status",{server:s.server,runId:r});s.evalRun=Lt(n?.run??n),s.evalCases=qt(await h("eval_cases",{server:s.server,runId:r}))}function ft(t,e){const r=/^(\d+)([a-z]+)$/i.exec(t.trim());return r?`${Number(r[1])*e}${r[2]}`:t}function Pt(t){const e=(Array.isArray(t?.comments)?t.comments:[]).map(a=>{try{return JSON.parse(String(a?.body??""))}catch{return null}}).filter(a=>a&&typeof a=="object"),r=new Map;for(const a of e)a.kind==="review_answer"&&r.set(String(a.review_id??""),a);const n=e.filter(a=>a.kind==="review_request").map(a=>{const o=String(a.review_id??""),d=r.get(o);return{reviewId:o,state:d?"answered":"open",traceId:a.trace_id?String(a.trace_id):null,question:String(a.question??""),answer:d?String(d.answer??""):null}});return n.sort((a,o)=>a.state===o.state?a.reviewId.localeCompare(o.reviewId):a.state==="open"?-1:1),n}async function z(t){const e=s.panels[t];e.loaded=!0,e.error=null;const r=s.server,n=s.lastWindow||"1h";try{if(t==="health"){const[a,o]=await Promise.all([h("query_summary",{server:r,last:n}),h("query_anomalies",{server:r,last:n,baseline:ft(n,4)})]);e.data={summary:a,anomalies:o}}else if(t==="topology")e.data=await h("query_topology",{server:r,last:n});else if(t==="automation"){const[a,o,d]=await Promise.all([h("list_alerts",{server:r}),h("alert_events",{server:r,limit:20}),h("list_score_rules",{server:r})]);e.data={alerts:a,events:o,scoreRules:d}}else t==="clusters"?e.data=await h("cluster_traces",{server:r,k:5}):t==="review"?e.data=Pt(await h("list_comments",{server:r,limit:500})):t==="sql"&&(e.data=await h("query_sql",{server:r,query:s.sqlQuery}))}catch(a){e.error=String(a),e.data=null}u()}async function Vt(t){s.tab=t,u(),s.panels[t].loaded||await z(t)}async function M(t){s.prevTab=s.tab==="detail"?s.prevTab:s.tab,s.tab="detail",s.currentTraceId=t,s.selectedWaterfallIdx=null,s.traceSpans=[],s.waterfallRows=[],s.comments=[],s.detailZoom={start:0,end:1},s.error=null,u();try{const[e,r]=await Promise.all([h("get_trace",{server:s.server,traceId:t}),h("get_comments",{server:s.server,traceId:t})]);s.traceSpans=G(e),s.waterfallRows=Wt(s.traceSpans),s.selectedWaterfallIdx=s.waterfallRows.length>0?0:null,s.comments=Y(r)}catch(e){s.error=String(e)}u()}async function jt(){if(!s.currentTraceId||!s.commentDraft.trim())return;const t=k();try{await h("add_comment",{server:s.server,request:{traceId:s.currentTraceId,body:s.commentDraft.trim(),author:"gui",spanId:t?.spanId??null}}),s.commentDraft="",s.comments=Y(await h("get_comments",{server:s.server,traceId:s.currentTraceId}))}catch(e){s.error=String(e)}u()}async function q(){s.error=null,s.connection="checking",s.streamId=ct(),u();try{await h("healthz",{server:s.server}),s.connection="loading",await Promise.all([Z(),tt(),et()]),await Ut(),s.connection="connected"}catch(t){s.connection="error",s.error=String(t)}u()}async function Ut(){await h("start_live_stream",{server:s.server,service:s.serviceFilter||null,status:s.statusFilter||null,streamId:s.streamId})}async function Bt(){j?.(),U?.(),j=await nt("tael://live-spans",t=>{if(!(t.payload.streamId!==s.streamId||s.paused))try{const e=G(JSON.parse(t.payload.data));if(e.length===0)return;pt(e),s.spans=[...e,...s.spans].slice(0,Et),s.error=null,u()}catch{}}),U=await nt("tael://live-status",t=>{t.payload.streamId===s.streamId&&(s.connection=t.payload.status,t.payload.message&&(s.error=t.payload.message),u())})}function st(){const t=K();return s.selectedTraceIdx==null?null:t[s.selectedTraceIdx]??null}function R(){const t=X();return s.selectedSpanIdx==null?null:t[s.selectedSpanIdx]??null}function k(){if(s.selectedWaterfallIdx==null)return null;const t=s.waterfallRows[s.selectedWaterfallIdx];return t?s.traceSpans[t.spanIdx]:null}function I(t,e){return`<button class="tab ${s.tab===t?"active":""}" data-tab="${t}">${e}</button>`}function vt(){p.innerHTML=`
    <div class="shell">
      <header class="topbar">
        <div class="brand">
          <span class="brand-mark">◆</span>
          <span class="brand-name">tael</span>
          <span class="conn"><span class="conn-dot ${i(s.connection)}"></span>${i(s.connection)}</span>
        </div>
        <div class="conn-controls">
          <label class="field"><span>server</span><input id="server-input" class="server-input" value="${i(s.server)}" /></label>
          <label class="field"><span>service</span><input id="service-input" class="small-input" placeholder="all" value="${i(s.serviceFilter)}" /></label>
          <label class="field"><span>status</span>
            <select id="status-input" class="small-input">
              <option value="" ${s.statusFilter===""?"selected":""}>all</option>
              <option value="ok" ${s.statusFilter==="ok"?"selected":""}>ok</option>
              <option value="error" ${s.statusFilter==="error"?"selected":""}>error</option>
            </select>
          </label>
          <label class="field"><span>window</span><input id="last-input" class="tiny-input" value="${i(s.lastWindow)}" /></label>
          <button id="connect-btn" class="primary">Connect</button>
          <button id="refresh-btn" title="Refresh">Refresh</button>
          <button id="pause-btn" class="${s.paused?"active":""}" title="Pause live ingest">${s.paused?"Resume":"Pause"}</button>
        </div>
      </header>
      <nav class="subnav">
        <div class="tabs">
          ${I("traces","Traces")}
          ${I("services","Services")}
          ${I("evals","Evals")}
          ${I("timeline","Timeline")}
          ${I("health","Health")}
          ${I("topology","Topology")}
          ${I("automation","Automation")}
          ${I("clusters","Clusters")}
          ${I("review","Review")}
          ${I("sql","SQL")}
          ${s.tab==="detail"?I("detail","Trace"):""}
        </div>
        <div class="filter-box">
          <input id="filter-input" placeholder="filter…" value="${i(s.textFilter)}" />
          ${s.textFilter?'<button id="clear-filter-btn">Clear</button>':""}
        </div>
      </nav>
      ${s.error?`<div class="error-bar">${i(s.error)}</div>`:'<div class="error-bar is-hidden"></div>'}
      <main class="workspace">${Ht()}</main>
      ${s.attrPickerOpen?ne():""}
      ${s.spanViewer?ie(s.spanViewer):""}
    </div>
  `,oe(),le()}function Ht(){return s.tab==="services"?te():s.tab==="evals"?ee():s.tab==="timeline"?re():s.tab==="detail"?ae():V(s.tab)?Zt(s.tab):Kt()}function L(t,e,r="muted"){return`
    <section class="pane table-pane full">
      <div class="pane-title"><span>${i(t)}</span></div>
      <p class="panel-note ${r}">${i(e)}</p>
    </section>
  `}function Zt(t){const e=s.panels[t];return e.error?L(t,e.error,"danger"):e.loaded?t==="health"?zt(e.data):t==="topology"?Qt(e.data):t==="automation"?Gt(e.data):t==="clusters"?Yt(e.data):t==="review"?Jt(e.data):Xt(e.data):L(t,"Loading…")}function m(t,e){const r=t?.[e];return typeof r=="number"&&Number.isFinite(r)?r:0}function $(t,e){const r=t?.[e];return r==null?"":typeof r=="string"?r:String(r)}function _(t,e){return Array.isArray(t?.[e])?t[e]:[]}function D(t){return t>.05?"danger":t>0?"warn":"ok"}function E(t,e,r=""){return`
    <div class="stat">
      <span class="stat-label">${i(t)}</span>
      <span class="stat-value ${r}">${i(e)}</span>
    </div>
  `}function zt(t){const e=t?.summary;if(!e)return L("Health","No summary yet.");const r=e.traces??{},n=e.logs??{},a=m(r,"error_rate"),o=_(t.anomalies,"anomalies"),d=_(e,"top_error_operations").slice(0,5).map(c=>`<tr>
        <td class="danger">${i(m(c,"error_count"))}</td>
        <td class="accent">${i($(c,"service"))}</td>
        <td>${i($(c,"operation"))}</td>
      </tr>`).join(""),l=o.map(c=>`<tr>
        <td class="accent">${i($(c,"service"))}</td>
        <td>${i($(c,"kind"))}</td>
        <td class="${$(c,"severity")==="high"?"danger":"warn"}">${i($(c,"severity"))}</td>
        <td>${m(c,"baseline").toFixed(2)}</td>
        <td>${m(c,"current").toFixed(2)}</td>
        <td>${i($(c,"description"))}</td>
      </tr>`).join("");return`
    <section class="pane table-pane full">
      <div class="pane-title"><span>Health</span><span>last ${i(s.lastWindow||"1h")}</span></div>
      <div class="stat-row">
        ${E("spans",String(m(r,"span_count")))}
        ${E("traces",String(m(r,"trace_count")))}
        ${E("errors",String(m(r,"error_count")),D(a))}
        ${E("error rate",`${(a*100).toFixed(2)}%`,D(a))}
        ${E("p50",`${m(r,"p50_ms").toFixed(1)}ms`)}
        ${E("p95",`${m(r,"p95_ms").toFixed(1)}ms`)}
        ${E("p99",`${m(r,"p99_ms").toFixed(1)}ms`)}
        ${E("logs",`${m(n,"total")} / ${m(n,"error")} err`)}
      </div>
      <div class="table-wrap">
        <div class="panel-subhead">Top error operations</div>
        <table>
          <thead><tr><th>Errors</th><th>Service</th><th>Operation</th></tr></thead>
          <tbody>${d||'<tr><td colspan="3" class="muted">No errors in this window.</td></tr>'}</tbody>
        </table>
        <div class="panel-subhead">Anomalies vs ${i(ft(s.lastWindow||"1h",4))} baseline</div>
        <table>
          <thead><tr><th>Service</th><th>Kind</th><th>Severity</th><th>Baseline</th><th>Current</th><th>Description</th></tr></thead>
          <tbody>${l||'<tr><td colspan="6" class="ok">Nothing regressed against the baseline window.</td></tr>'}</tbody>
        </table>
      </div>
    </section>
  `}function Qt(t){const e=_(t,"edges");if(e.length===0)return L("Topology","No parent/child edges in this window. A single-service trace has no graph.");const r=m(t,"spans_with_parent_outside_window"),n=e.map(a=>{const o=m(a,"error_rate");return`<tr>
        <td class="accent">${i($(a,"from"))}</td>
        <td class="muted">→</td>
        <td class="accent">${i($(a,"to"))}</td>
        <td>${m(a,"calls")}</td>
        <td class="${D(o)}">${m(a,"errors")}</td>
        <td class="${D(o)}">${(o*100).toFixed(1)}%</td>
        <td>${m(a,"avg_duration_ms").toFixed(1)}ms</td>
      </tr>`}).join("");return`
    <section class="pane table-pane full">
      <div class="pane-title">
        <span>Topology</span>
        <span>${e.length} edges over ${m(t,"spans_examined")} spans${r>0?` · <b class="warn">${r} with a parent outside the window</b>`:""}</span>
      </div>
      <div class="table-wrap">
        <table>
          <thead><tr><th>From</th><th></th><th>To</th><th>Calls</th><th>Errors</th><th>Rate</th><th>Avg</th></tr></thead>
          <tbody>${n}</tbody>
        </table>
      </div>
    </section>
  `}function Gt(t){const e=_(t?.alerts,"alerts"),r=_(t?.events,"events"),n=_(t?.scoreRules,"rules"),a=e.map(l=>{const c=$(l,"state");return`<tr>
        <td class="accent">${i($(l,"name"))}</td>
        <td class="${c==="firing"?"danger":c==="pending"?"warn":"ok"}">${i(c)}</td>
        <td>${m(l,"for_seconds")}s</td>
        <td>${_(l,"sinks").length}</td>
        <td class="mono">${i($(l,"query"))}</td>
      </tr>`}).join(""),o=r.map(l=>{const c=$(l,"state");return`<tr>
        <td class="muted">${i(J($(l,"at")))}</td>
        <td class="accent">${i($(l,"rule"))}</td>
        <td class="${c==="firing"?"danger":"ok"}">${i($(l,"previous_state"))} → ${i(c)}</td>
        <td>${_(l,"matched").length} series</td>
      </tr>`}).join(""),d=n.map(l=>{const c=l?.status??{},f=$(c,"last_error");return`<tr>
        <td class="accent">${i($(l,"name"))}</td>
        <td>${(m(l,"sample")*100).toFixed(0)}%</td>
        <td>${m(c,"scored")}</td>
        <td class="danger">${i(f||"—")}</td>
        <td class="mono">${i($(l,"command"))}</td>
      </tr>`}).join("");return`
    <section class="pane table-pane full">
      <div class="pane-title"><span>Automation</span><span>${e.length} alert rules · ${n.length} scoring rules</span></div>
      <div class="table-wrap">
        <div class="panel-subhead">Alert rules (${e.length})</div>
        <table>
          <thead><tr><th>Rule</th><th>State</th><th>For</th><th>Sinks</th><th>Query</th></tr></thead>
          <tbody>${a||'<tr><td colspan="5" class="muted">No alert rules. Create one with <code>tael alert create</code>.</td></tr>'}</tbody>
        </table>
        <div class="panel-subhead">Alert feed (${r.length})</div>
        <table>
          <thead><tr><th>When</th><th>Rule</th><th>Transition</th><th>Matched</th></tr></thead>
          <tbody>${o||'<tr><td colspan="4" class="ok">Nothing has fired.</td></tr>'}</tbody>
        </table>
        <div class="panel-subhead">Scoring rules (${n.length})</div>
        <table>
          <thead><tr><th>Rule</th><th>Sample</th><th>Scored</th><th>Last error</th><th>Command</th></tr></thead>
          <tbody>${d||'<tr><td colspan="5" class="muted">No scoring rules. Create one with <code>tael score rule create</code>.</td></tr>'}</tbody>
        </table>
      </div>
    </section>
  `}function Yt(t){const e=_(t,"clusters");if(e.length===0)return L("Clusters","Nothing embedded yet. Run `tael embed --command <your embedder>` first.");const r=e.map(n=>{const a=m(n,"cohesion"),o=a>=.85?"ok":a>=.7?"warn":"danger",d=$(n,"exemplar");return`<tr data-cluster-trace="${i(d)}">
        <td class="accent">#${m(n,"id")}</td>
        <td>${m(n,"size")}</td>
        <td class="${o}">${a.toFixed(3)}</td>
        <td class="danger">${a>=.7?"":"weak"}</td>
        <td class="mono muted">${i(d)}</td>
      </tr>`}).join("");return`
    <section class="pane table-pane full">
      <div class="pane-title">
        <span>Clusters</span>
        <span>${e.length} over ${m(t,"corpus_size")} embedded traces · cohesion below 0.7 is weak</span>
      </div>
      <div class="table-wrap">
        <table>
          <thead><tr><th>Cluster</th><th>Size</th><th>Cohesion</th><th></th><th>Exemplar (click to open)</th></tr></thead>
          <tbody>${r}</tbody>
        </table>
      </div>
    </section>
  `}function Jt(t){if(!t||t.length===0)return L("Review queue","Nothing waiting on a human.","ok");const e=t.filter(n=>n.state==="open").length,r=t.map(n=>`<tr ${n.traceId?`data-review-trace="${i(n.traceId)}"`:""}>
        <td class="${n.state==="open"?"warn":"ok"}">${i(n.state)}</td>
        <td class="mono muted">${i(n.traceId?O(n.traceId,12):"—")}</td>
        <td>${i(n.question)}</td>
        <td class="ok">${i(n.answer??"")}</td>
      </tr>`).join("");return`
    <section class="pane table-pane full">
      <div class="pane-title"><span>Review queue</span><span>${e} open of ${t.length}</span></div>
      <div class="table-wrap">
        <table>
          <thead><tr><th>State</th><th>Trace</th><th>Question (click to open)</th><th>Answer</th></tr></thead>
          <tbody>${r}</tbody>
        </table>
      </div>
    </section>
  `}function Xt(t){const e=_(t,"rows"),r=e.length>0&&e[0]&&typeof e[0]=="object"?Object.keys(e[0]):[],n=e.map(a=>`<tr>${r.map(o=>{const d=a?.[o],l=d==null?"":typeof d=="string"?d:JSON.stringify(d);return`<td>${i(l)}</td>`}).join("")}</tr>`).join("");return`
    <section class="pane table-pane full">
      <div class="pane-title"><span>SQL</span><span>${e.length} rows</span></div>
      <div class="sql-bar">
        <textarea id="sql-input" class="sql-input" rows="3" spellcheck="false">${i(s.sqlQuery)}</textarea>
        <button id="sql-run-btn" class="primary">Run</button>
      </div>
      <div class="table-wrap">
        <table>
          <thead><tr>${r.map(a=>`<th>${i(a)}</th>`).join("")||"<th></th>"}</tr></thead>
          <tbody>${n||'<tr><td class="muted">No rows.</td></tr>'}</tbody>
        </table>
      </div>
    </section>
  `}function Kt(){const t=X(),e=R(),r=s.pinnedColumns.map(n=>`<th>${i(n)}</th>`).join("");return`
    <section class="split vertical">
      <div class="pane table-pane">
        <div class="pane-title">
          <span>Traces</span>
          <span>${t.length}/${s.spans.length}</span>
        </div>
        <div class="table-wrap">
          <table>
            <thead><tr><th>Time</th><th>Service</th><th>Operation</th><th>Duration</th><th>Status</th><th>Trace ID</th>${r}</tr></thead>
            <tbody>
              ${t.map((n,a)=>`
                <tr class="${s.selectedSpanIdx===a?"selected":""}" data-span-idx="${a}">
                  <td class="muted">${i(J(n.startTime))}</td>
                  <td style="color:${C(n.service)}">${i(n.service)}</td>
                  <td>${i(n.operation)}</td>
                  <td class="${F(n.durationMs)}">${n.durationMs.toFixed(0)}ms</td>
                  <td class="${N(n.status)}">${i(n.status)}</td>
                  <td class="mono muted">${i(O(n.traceId))}</td>
                  ${s.pinnedColumns.map(o=>{const d=Ft(n,o);return`<td class="${d?"attr-cell":"muted"}">${i(d||"-")}</td>`}).join("")}
                </tr>
              `).join("")}
            </tbody>
          </table>
        </div>
      </div>
      <aside class="pane detail-pane">${e?mt(e):'<div class="empty">No span selected.</div>'}</aside>
    </section>
  `}function mt(t){return`
    <div class="pane-title">
      <span>Span</span>
      <div class="button-row">
        <button id="pin-columns-btn">Columns</button>
        <button id="view-span-btn">View</button>
        <button id="open-selected-trace-btn">Open Trace</button>
      </div>
    </div>
    <dl class="properties">
      <dt>trace_id</dt><dd class="mono">${i(t.traceId)}</dd>
      <dt>span_id</dt><dd class="mono">${i(t.spanId)}</dd>
      <dt>parent</dt><dd class="mono">${i(t.parentSpanId??"none")}</dd>
      <dt>service</dt><dd style="color:${C(t.service)}">${i(t.service)}</dd>
      <dt>operation</dt><dd>${i(t.operation)}</dd>
      <dt>status</dt><dd class="${N(t.status)}">${i(t.status)}</dd>
      <dt>duration</dt><dd class="${F(t.durationMs)}">${t.durationMs.toFixed(2)}ms</dd>
      <dt>start</dt><dd>${i(t.startTime)}</dd>
    </dl>
    <pre class="json-view">${i(JSON.stringify({attributes:t.attributes,events:t.events},null,2))}</pre>
  `}function te(){return`
    <section class="pane table-pane full">
      <div class="pane-title"><span>Services</span><span>${s.services.length}</span></div>
      <div class="table-wrap">
        <table>
          <thead><tr><th>Service</th><th>Spans</th><th>Traces</th><th>Avg Duration</th><th>Error Rate</th></tr></thead>
          <tbody>
            ${s.services.map((t,e)=>`
              <tr class="${s.selectedServiceIdx===e?"selected":""}" data-service-idx="${e}">
                <td style="color:${C(t.name)}">${i(t.name)}</td>
                <td>${t.spanCount}</td>
                <td>${t.traceCount}</td>
                <td class="${F(t.avgDurationMs)}">${t.avgDurationMs.toFixed(1)}ms</td>
                <td class="${t.errorRate>.05?"danger":t.errorRate>0?"warn":"ok"}">${(t.errorRate*100).toFixed(1)}%</td>
              </tr>
            `).join("")}
          </tbody>
        </table>
      </div>
    </section>
  `}function ee(){const t=s.evalRun,e=H(),r=s.selectedEvalIdx==null?null:e[s.selectedEvalIdx];if(!t)return'<section class="pane full"><div class="empty">No eval runs found.</div></section>';const n=typeof t.avgScores.correctness=="number"?t.avgScores.correctness.toFixed(3):"-";return`
    <section class="split vertical eval-layout">
      <div class="pane run-strip">
        <div class="run-stat grow">
          <span class="run-stat-label">Suite</span>
          <span class="run-stat-value">${i(t.suiteId)}</span>
          <span class="run-stat-sub mono">${i(t.runId)}</span>
        </div>
        <div class="run-stat">
          <span class="run-stat-label">Status</span>
          <span class="run-stat-value ${N(t.status)}">${i(t.status)}</span>
        </div>
        <div class="run-stat">
          <span class="run-stat-label">Cases</span>
          <span class="run-stat-value">${t.observedCases}<span class="run-stat-sub"> / ${t.caseCount??"?"}</span></span>
        </div>
        <div class="run-stat">
          <span class="run-stat-label">Pass</span>
          <span class="run-stat-value ok">${t.passedCases}</span>
        </div>
        <div class="run-stat">
          <span class="run-stat-label">Fail</span>
          <span class="run-stat-value ${t.failedCases>0?"danger":""}">${t.failedCases}</span>
        </div>
        <div class="run-stat">
          <span class="run-stat-label">Avg score</span>
          <span class="run-stat-value">${n}</span>
        </div>
        <div class="run-stat">
          <span class="run-stat-label">Cost</span>
          <span class="run-stat-value">$${t.costUsd.toFixed(4)}</span>
        </div>
        <button id="failures-only-btn" class="spacer ${s.evalFailuresOnly?"active":""}">Failures</button>
      </div>
      <div class="pane table-pane">
        <div class="pane-title"><span>Cases</span><span>${e.length}</span></div>
        <div class="table-wrap">
          <table>
            <thead><tr><th>Status</th><th>Case</th><th>Score</th><th>Cost</th><th>Duration</th><th>Trace</th></tr></thead>
            <tbody>
              ${e.map((a,o)=>{const d=typeof a.scores.correctness=="number"?a.scores.correctness.toFixed(3):Object.values(a.scores).find(l=>typeof l=="number")?.toString()??"-";return`
                  <tr class="${s.selectedEvalIdx===o?"selected":""}" data-eval-idx="${o}">
                    <td class="${N(a.status)}">${i(a.status.toUpperCase())}</td>
                    <td>${i(a.caseId)}</td>
                    <td>${i(d)}</td>
                    <td>${a.costUsd.toFixed(4)}</td>
                    <td>${a.durationMs==null?"-":`${a.durationMs.toFixed(0)}ms`}</td>
                    <td class="mono muted">${i(a.traceId?O(a.traceId,12):"-")}</td>
                  </tr>
                `}).join("")}
            </tbody>
          </table>
        </div>
      </div>
      <aside class="pane detail-pane">${r?se(r):'<div class="empty">No case selected.</div>'}</aside>
    </section>
  `}function se(t){return`
    <div class="pane-title">
      <span>${i(t.caseId)}</span>
      ${t.traceId?'<button id="open-eval-trace-btn">Open Trace</button>':""}
    </div>
    <dl class="properties">
      <dt>status</dt><dd class="${N(t.status)}">${i(t.status)}</dd>
      <dt>trace</dt><dd class="mono">${i(t.traceId??"-")}</dd>
      <dt>duration</dt><dd>${t.durationMs==null?"-":`${t.durationMs.toFixed(1)}ms`}</dd>
      <dt>cost</dt><dd>$${t.costUsd.toFixed(4)}</dd>
    </dl>
    <pre class="json-view">${i(JSON.stringify(t.scores,null,2))}</pre>
    ${t.comments.length?`<div class="comment-list">${t.comments.map(ht).join("")}</div>`:""}
  `}function re(){const t=st();return`
    <section class="split vertical">
      <div class="pane timeline-pane">
        <div class="pane-title">
          <span>Live Timeline</span>
          <span>${K().length}/${s.liveTraces.length} traces</span>
        </div>
        <canvas id="timeline-canvas" class="timeline-canvas"></canvas>
      </div>
      <aside class="pane detail-pane">
        ${t?`
          <div class="pane-title"><span>Trace</span><button id="open-selected-live-trace-btn">Open Trace</button></div>
          <dl class="properties">
            <dt>trace_id</dt><dd class="mono">${i(t.traceId)}</dd>
            <dt>service</dt><dd style="color:${C(t.service)}">${i(t.service)}</dd>
            <dt>operation</dt><dd>${i(t.operation)}</dd>
            <dt>status</dt><dd class="${t.hasError?"danger":"ok"}">${t.hasError?"error":"ok"}</dd>
            <dt>duration</dt><dd class="${F(t.durationMs)}">${t.durationMs.toFixed(2)}ms</dd>
            <dt>spans</dt><dd>${t.spanCount}</dd>
          </dl>
        `:'<div class="empty">No trace selected.</div>'}
      </aside>
    </section>
  `}function ae(){const t=k();return`
    <section class="detail-grid">
      <div class="pane waterfall-pane">
        <div class="pane-title">
          <span>${i(s.currentTraceId?`Trace ${O(s.currentTraceId)}`:"Trace")}</span>
          <button id="back-btn">Back</button>
        </div>
        <canvas id="waterfall-canvas" class="waterfall-canvas"></canvas>
      </div>
      <aside class="pane span-side">
        ${t?mt(t):'<div class="empty">No span selected.</div>'}
      </aside>
      <section class="pane comments-pane">
        <div class="pane-title"><span>Comments</span><span>${s.comments.length}</span></div>
        <div class="comment-list">${s.comments.map(ht).join("")||'<div class="empty compact">No comments.</div>'}</div>
        <div class="comment-form">
          <input id="comment-input" value="${i(s.commentDraft)}" />
          <button id="submit-comment-btn">Add</button>
        </div>
      </section>
    </section>
  `}function ht(t){const e=J(t.createdAt).slice(0,8);return`
    <div class="comment">
      <span class="muted">${i(e)}</span>
      <strong>${i(t.author)}</strong>
      ${t.spanId?`<span class="mono muted">${i(O(t.spanId,8))}</span>`:""}
      <p>${i(t.body)}</p>
    </div>
  `}function ne(){const t=Ot();return`
    <div class="overlay">
      <section class="modal attr-modal">
        <div class="modal-title">
          <span>Pin Attribute Columns</span>
          <button id="close-attr-picker-btn">Close</button>
        </div>
        <div class="modal-body">
          ${t.length?t.map(e=>`
                <label class="check-row">
                  <input type="checkbox" data-attr-key="${i(e)}" ${s.pinnedColumns.includes(e)?"checked":""} />
                  <span class="mono">${i(e)}</span>
                </label>
              `).join(""):'<div class="empty compact">No attributes found.</div>'}
        </div>
      </section>
    </div>
  `}function ie(t){return`
    <div class="overlay">
      <section class="modal span-modal">
        <div class="modal-title">
          <span>${i(t.service)} / ${i(t.operation)}</span>
          <button id="close-span-viewer-btn">Close</button>
        </div>
        <div class="modal-body split-modal">
          <dl class="properties modal-properties">
            <dt>trace_id</dt><dd class="mono">${i(t.traceId)}</dd>
            <dt>span_id</dt><dd class="mono">${i(t.spanId)}</dd>
            <dt>parent</dt><dd class="mono">${i(t.parentSpanId??"none")}</dd>
            <dt>service</dt><dd style="color:${C(t.service)}">${i(t.service)}</dd>
            <dt>operation</dt><dd>${i(t.operation)}</dd>
            <dt>status</dt><dd class="${N(t.status)}">${i(t.status)}</dd>
            <dt>duration</dt><dd class="${F(t.durationMs)}">${t.durationMs.toFixed(2)}ms</dd>
            <dt>start</dt><dd>${i(t.startTime)}</dd>
          </dl>
          <pre class="json-view modal-json">${i(JSON.stringify({attributes:t.attributes,events:t.events},null,2))}</pre>
        </div>
      </section>
    </div>
  `}function oe(){p.querySelector("#server-input")?.addEventListener("change",e=>{s.server=e.currentTarget.value.trim()}),p.querySelector("#service-input")?.addEventListener("change",e=>{s.serviceFilter=e.currentTarget.value.trim(),q()}),p.querySelector("#status-input")?.addEventListener("change",e=>{s.statusFilter=e.currentTarget.value,q()}),p.querySelector("#last-input")?.addEventListener("change",e=>{s.lastWindow=e.currentTarget.value.trim()||"1h",Z().catch(r=>s.error=String(r)).finally(u)}),p.querySelector("#filter-input")?.addEventListener("input",e=>{s.textFilter=e.currentTarget.value,s.selectedSpanIdx=null,s.selectedTraceIdx=null,s.selectedEvalIdx=null,u()}),p.querySelector("#clear-filter-btn")?.addEventListener("click",()=>{s.textFilter="",u()}),p.querySelector("#connect-btn")?.addEventListener("click",q),p.querySelector("#refresh-btn")?.addEventListener("click",()=>{if(V(s.tab)){z(s.tab);return}Promise.all([Z(),tt(),et()]).catch(e=>s.error=String(e)).finally(u)}),p.querySelector("#pause-btn")?.addEventListener("click",()=>{s.paused=!s.paused,u()}),p.querySelectorAll("[data-tab]").forEach(e=>{e.addEventListener("click",()=>{const r=e.dataset.tab;if(V(r)){Vt(r);return}s.tab=r,u()})}),p.querySelectorAll("[data-cluster-trace]").forEach(e=>{e.addEventListener("click",()=>{M(e.dataset.clusterTrace)})}),p.querySelectorAll("[data-review-trace]").forEach(e=>{e.addEventListener("click",()=>{M(e.dataset.reviewTrace)})});const t=p.querySelector("#sql-input");t?.addEventListener("input",()=>{s.sqlQuery=t.value}),p.querySelector("#sql-run-btn")?.addEventListener("click",()=>{s.sqlQuery.trim()&&z("sql")}),p.querySelectorAll("[data-span-idx]").forEach(e=>{e.addEventListener("click",()=>{s.selectedSpanIdx=Number(e.dataset.spanIdx),u()}),e.addEventListener("dblclick",()=>{const r=X()[Number(e.dataset.spanIdx)];r&&M(r.traceId)})}),p.querySelector("#open-selected-trace-btn")?.addEventListener("click",()=>{const e=R()??k();e&&M(e.traceId)}),p.querySelector("#pin-columns-btn")?.addEventListener("click",()=>{s.attrPickerOpen=!0,u()}),p.querySelector("#view-span-btn")?.addEventListener("click",()=>{const e=R()??k();e&&(s.spanViewer=e,u())}),p.querySelector("#close-attr-picker-btn")?.addEventListener("click",()=>{s.attrPickerOpen=!1,u()}),p.querySelectorAll("[data-attr-key]").forEach(e=>{e.addEventListener("change",()=>{const r=e.dataset.attrKey;r&&Dt(r),u()})}),p.querySelector("#close-span-viewer-btn")?.addEventListener("click",()=>{s.spanViewer=null,u()}),p.querySelectorAll("[data-service-idx]").forEach(e=>{e.addEventListener("click",()=>{const r=s.services[Number(e.dataset.serviceIdx)];r&&(s.selectedServiceIdx=Number(e.dataset.serviceIdx),s.serviceFilter=r.name,s.tab="traces",q())})}),p.querySelector("#failures-only-btn")?.addEventListener("click",()=>{s.evalFailuresOnly=!s.evalFailuresOnly,s.selectedEvalIdx=null,u()}),p.querySelectorAll("[data-eval-idx]").forEach(e=>{e.addEventListener("click",()=>{s.selectedEvalIdx=Number(e.dataset.evalIdx),u()}),e.addEventListener("dblclick",()=>{const r=H()[Number(e.dataset.evalIdx)];r?.traceId&&M(r.traceId)})}),p.querySelector("#open-eval-trace-btn")?.addEventListener("click",()=>{const e=s.selectedEvalIdx==null?null:H()[s.selectedEvalIdx];e?.traceId&&M(e.traceId)}),p.querySelector("#open-selected-live-trace-btn")?.addEventListener("click",()=>{const e=st();e&&M(e.traceId)}),p.querySelector("#back-btn")?.addEventListener("click",()=>{s.tab=s.prevTab,u()}),p.querySelector("#comment-input")?.addEventListener("input",e=>{s.commentDraft=e.currentTarget.value}),p.querySelector("#submit-comment-btn")?.addEventListener("click",jt)}function le(){const t=p.querySelector("#timeline-canvas");t&&de(t);const e=p.querySelector("#waterfall-canvas");e&&ce(e)}function bt(t){const e=t.getBoundingClientRect(),r=window.devicePixelRatio||1;t.width=Math.max(1,Math.floor(e.width*r)),t.height=Math.max(1,Math.floor(e.height*r));const n=t.getContext("2d");if(!n)throw new Error("2d canvas unavailable");return n.scale(r,r),n.clearRect(0,0,e.width,e.height),n}function de(t){const e=K(),r=bt(t),n=t.getBoundingClientRect(),a=260,o=26,d=34,l=Math.max(n.width-a-96,1),f=e.reduce((v,g)=>Math.max(v,g.endTimeMs),0)-s.timelineWindowMs,b=f+s.timelineWindowMs*s.liveZoom.start,y=f+s.timelineWindowMs*s.liveZoom.end,S=Math.max(y-b,1);r.fillStyle=Q,r.fillRect(0,0,n.width,n.height),$t(r,a,12,l,b,y);const x=e.filter(v=>v.endTimeMs>=b&&v.startTimeMs<=y);x.forEach((v,g)=>{const w=d+g*o;if(w>n.height-o)return;const St=e.indexOf(v)===s.selectedTraceIdx;yt(r,0,w-3,n.width,o,St),r.fillStyle=C(v.service),r.font=it,r.fillText(`${v.service} ${v.operation}`.slice(0,34),18,w+13);const rt=a+T((v.startTimeMs-b)/S,0,1)*l,It=Math.max(2,v.durationMs/S*l);r.fillStyle=v.hasError?dt:C(v.service),wt(r,rt,w,Math.min(It,a+l-rt),14,3),r.fill(),r.fillStyle=ot,r.fillText(`${v.durationMs.toFixed(0)}ms`,a+l+14,w+12),r.fillStyle=lt,r.fillText(String(v.spanCount),a+l+68,w+12)}),t.onmousemove=v=>{const g=Math.floor((v.offsetY-d)/o),w=x[g];t.title=w?`${w.service} ${w.operation} ${w.durationMs.toFixed(1)}ms`:""},t.onclick=v=>{const g=Math.floor((v.offsetY-d)/o),w=x[g];w&&(s.selectedTraceIdx=e.indexOf(w),u())},t.ondblclick=()=>{const v=st();v&&M(v.traceId)},t.onwheel=v=>{v.preventDefault();const g=v.deltaY>0?1.18:.84;gt(s.liveZoom,g,v.offsetX/n.width),u()}}function ce(t){const e=bt(t),r=t.getBoundingClientRect(),n=s.waterfallRows,a=300,o=28,d=36,l=Math.max(r.width-a-92,1);e.fillStyle=Q,e.fillRect(0,0,r.width,r.height),$t(e,a,12,l,s.detailZoom.start,s.detailZoom.end,!0),n.forEach((c,f)=>{const b=s.traceSpans[c.spanIdx],y=d+f*o;if(y>r.height-o)return;const S=s.selectedWaterfallIdx===f;yt(e,0,y-4,r.width,o,S),e.font=it,e.fillStyle=C(b.service),e.fillText(`${" ".repeat(c.depth*2)}${b.service} ${b.operation}`.slice(0,42),18,y+13);const x=s.detailZoom.end-s.detailZoom.start,v=a+(c.offsetPct-s.detailZoom.start)/x*l,g=Math.max(2,c.widthPct/x*l);v+g<a||v>a+l||(e.fillStyle=b.status==="error"?dt:C(b.service),wt(e,T(v,a,a+l),y,Math.min(g,a+l-v),15,3),e.fill(),e.fillStyle=ot,e.fillText(`${b.durationMs.toFixed(0)}ms`,a+l+14,y+12))}),t.onclick=c=>{const f=Math.floor((c.offsetY-d)/o);n[f]&&(s.selectedWaterfallIdx=f,u())},t.ondblclick=()=>{const c=k();c&&(s.selectedSpanIdx=s.spans.findIndex(f=>f.spanId===c.spanId))},t.onwheel=c=>{c.preventDefault(),gt(s.detailZoom,c.deltaY>0?1.18:.84,c.offsetX/r.width),u()}}function $t(t,e,r,n,a,o,d=!1){t.strokeStyle=At,t.fillStyle=lt,t.font=Mt,t.beginPath(),t.moveTo(e,r+12),t.lineTo(e+n,r+12),t.stroke();for(let l=0;l<=4;l+=1){const c=e+n*l/4;t.beginPath(),t.moveTo(c,r+7),t.lineTo(c,r+17),t.stroke();const f=a+(o-a)*l/4,b=d?`${Math.round(f*100)}%`:l===4?"now":`-${Math.round((o-f)/1e3)}s`;t.fillText(b,c+4,r+7)}}function yt(t,e,r,n,a,o){t.fillStyle=o?xt:r%56===0?kt:Q,t.fillRect(e,r,n,a)}function wt(t,e,r,n,a,o){const d=Math.min(o,n/2,a/2);t.beginPath(),t.moveTo(e+d,r),t.arcTo(e+n,r,e+n,r+a,d),t.arcTo(e+n,r+a,e,r+a,d),t.arcTo(e,r+a,e,r,d),t.arcTo(e,r,e+n,r,d),t.closePath()}function gt(t,e,r){const n=t.end-t.start,a=T(n*e,.03,1),o=t.start+n*T(r,0,1);t.start=T(o-a*r,0,1-a),t.end=t.start+a}window.addEventListener("keydown",t=>{if(!(t.target instanceof HTMLInputElement||t.target instanceof HTMLSelectElement)){if(t.key==="Escape"&&s.spanViewer){s.spanViewer=null,u();return}if(t.key==="Escape"&&s.attrPickerOpen){s.attrPickerOpen=!1,u();return}if(t.key==="1"&&(s.tab="traces"),t.key==="2"&&(s.tab="services"),t.key==="3"&&(s.tab="evals"),t.key==="4"&&(s.tab="timeline"),t.key==="Escape"&&s.tab==="detail"&&(s.tab=s.prevTab),t.key===" "&&(s.paused=!s.paused),t.key==="a"&&(R()||k())&&(s.attrPickerOpen=!0),t.key==="v"){const e=R()??k();e&&(s.spanViewer=e)}u()}});window.addEventListener("resize",u);async function ue(){vt();try{const t=await h("initial_server");t.trim()&&(s.server=t.trim())}catch(t){console.warn("failed to load initial server",t)}try{await Bt()}catch(t){s.error=`failed to install live listeners: ${String(t)}`,u()}q()}ue();B=window.setInterval(()=>{s.connection==="connected"&&Promise.all([tt(),et()]).catch(t=>{s.error=String(t),u()})},5e3);window.addEventListener("beforeunload",()=>{j?.(),U?.(),B!=null&&window.clearInterval(B)});
