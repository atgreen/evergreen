#!/usr/bin/env python3
"""jitrec — record a bliss JIT/GC event stream into SQLite and explore it.

The static HTML viewer (tools/event-viewer) embeds a whole run in one page, which
does not scale: bliss's in-memory ring drops events past ~65k, and a large
program's disassembly makes a multi-megabyte file. This tool takes the
record-then-explore path (JFR/JDK-Mission-Control model):

  1. bliss STREAMS an unbounded NDJSON event log:
         BLISS_EVENTS_STREAM=run.ndjson bliss-cli --load big-system.lisp
     (`e` records live per event, then bounded `sym`/`fn` records at exit).

  2. jitrec INGESTS that log into an indexed SQLite database:
         scripts/jitrec.py ingest run.ndjson run.db

  3. jitrec SERVES a web explorer that queries the DB on demand — the per-
     function table and timeline are computed in SQL, and disassembly is fetched
     one function at a time, so the browser never holds the whole run:
         scripts/jitrec.py serve run.db 8765   # then open http://localhost:8765

`ingest` also accepts `-` to read NDJSON from stdin (live: `... | tee run.ndjson`
or pipe straight through). No third-party dependencies — stdlib sqlite3 +
http.server. A Rust `bliss-jitrec` binary is the natural productionization.
"""
import sys
import os
import json
import sqlite3

SCHEMA = """
CREATE TABLE IF NOT EXISTS events(
    seq  INTEGER PRIMARY KEY,
    ns   INTEGER NOT NULL,
    kind TEXT    NOT NULL,
    sym  INTEGER NOT NULL,
    a    INTEGER NOT NULL,
    b    INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_events_kind ON events(kind);
CREATE INDEX IF NOT EXISTS ix_events_sym  ON events(sym);
CREATE INDEX IF NOT EXISTS ix_events_ns   ON events(ns);

CREATE TABLE IF NOT EXISTS symbols(
    id   INTEGER PRIMARY KEY,
    name TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS functions(
    sym   INTEGER PRIMARY KEY,
    name  TEXT NOT NULL,
    src   TEXT,
    t0    TEXT,
    t1    TEXT,
    t2    TEXT,
    t1map TEXT,   -- JSON array
    t2map TEXT    -- JSON array
);
CREATE INDEX IF NOT EXISTS ix_functions_name ON functions(name);

CREATE TABLE IF NOT EXISTS meta(k TEXT PRIMARY KEY, v TEXT);
"""


def ingest(src, db_path):
    con = sqlite3.connect(db_path)
    con.executescript(SCHEMA)
    cur = con.cursor()
    stream = sys.stdin if src == "-" else open(src, encoding="utf-8")
    n_ev = n_sym = n_fn = n_bad = 0
    ev_batch = []

    def flush():
        if ev_batch:
            cur.executemany(
                "INSERT OR REPLACE INTO events(seq,ns,kind,sym,a,b) VALUES(?,?,?,?,?,?)",
                ev_batch,
            )
            ev_batch.clear()

    for line in stream:
        line = line.strip()
        if not line:
            continue
        try:
            r = json.loads(line)
        except json.JSONDecodeError:
            n_bad += 1
            continue
        t = r.get("t")
        if t == "e":
            ev_batch.append((r["s"], r["n"], r["k"], r["y"], r["a"], r["b"]))
            n_ev += 1
            if len(ev_batch) >= 5000:
                flush()
        elif t == "sym":
            cur.execute(
                "INSERT OR REPLACE INTO symbols(id,name) VALUES(?,?)",
                (r["id"], r["name"]),
            )
            n_sym += 1
        elif t == "fn":
            cur.execute(
                "INSERT OR REPLACE INTO functions(sym,name,src,t0,t1,t2,t1map,t2map)"
                " VALUES(?,?,?,?,?,?,?,?)",
                (
                    r["sym"], r["name"], r.get("src"), r.get("t0"),
                    r.get("t1"), r.get("t2"),
                    json.dumps(r.get("t1map")) if "t1map" in r else None,
                    json.dumps(r.get("t2map")) if "t2map" in r else None,
                ),
            )
            n_fn += 1
    flush()
    con.commit()
    con.close()
    if stream is not sys.stdin:
        stream.close()
    print(
        f"[jitrec] ingested {n_ev} events, {n_sym} symbols, {n_fn} functions "
        f"into {db_path}" + (f" ({n_bad} unparsable lines skipped)" if n_bad else ""),
        file=sys.stderr,
    )


# ── serve ──────────────────────────────────────────────────────────────────

def q(con, sql, args=()):
    cur = con.execute(sql, args)
    cols = [c[0] for c in cur.description]
    return [dict(zip(cols, row)) for row in cur.fetchall()]


def api_summary(con):
    kinds = {r["kind"]: r["n"] for r in q(con, "SELECT kind, COUNT(*) n FROM events GROUP BY kind")}
    span = q(con, "SELECT MIN(ns) lo, MAX(ns) hi FROM events")[0]
    gc = q(con, "SELECT COALESCE(SUM(a),0) us FROM events WHERE kind IN ('gc-minor','gc-major')")[0]
    nfn = q(con, "SELECT COUNT(*) n FROM functions")[0]["n"]
    return {
        "kinds": kinds,
        "total": sum(kinds.values()),
        "span_ns": [span["lo"] or 0, span["hi"] or 0],
        "gc_pause_us": gc["us"],
        "functions": nfn,
    }


def api_functions(con):
    # Per-function JIT activity, aggregated in SQL (scales to any event count).
    rows = q(con, """
        SELECT e.sym sym,
               COALESCE(s.name, '#<sym '||e.sym||'>') name,
               SUM(e.kind='compile' AND e.a<2) t1,
               SUM(e.kind='compile' AND e.a>=2) t2,
               SUM(e.kind='deopt') deopt,
               SUM(e.kind='osr') osr
        FROM events e LEFT JOIN symbols s ON s.id=e.sym
        WHERE e.sym <> 4294967295
        GROUP BY e.sym
        ORDER BY (t1+t2+deopt+osr) DESC
    """)
    return {"functions": rows}


def api_timeline(con, buckets):
    span = q(con, "SELECT MIN(ns) lo, MAX(ns) hi FROM events")[0]
    lo, hi = span["lo"] or 0, span["hi"] or 1
    width = max(1, (hi - lo) // buckets + 1)
    rows = q(con, f"""
        SELECT (ns-{lo})/{width} bucket, kind, COUNT(*) n
        FROM events GROUP BY bucket, kind ORDER BY bucket
    """)
    return {"lo": lo, "hi": hi, "width": width, "buckets": buckets, "rows": rows}


def api_deopts(con, limit):
    return {"deopts": q(con, """
        SELECT e.ns ns, COALESCE(s.name,'#<sym '||e.sym||'>') name, e.a reason, e.b n
        FROM events e LEFT JOIN symbols s ON s.id=e.sym
        WHERE e.kind='deopt' ORDER BY e.ns LIMIT ?""", (limit,))}


def api_function(con, sym):
    rows = q(con, "SELECT * FROM functions WHERE sym=?", (sym,))
    if not rows:
        return {"error": "not found"}
    f = rows[0]
    for k in ("t1map", "t2map"):
        f[k] = json.loads(f[k]) if f[k] else None
    return f


DEOPT_REASON = {0: "guard", 1: "phase-change", 2: "blacklist"}


def serve(db_path, port):
    import http.server
    con = sqlite3.connect(db_path, check_same_thread=False)

    class H(http.server.BaseHTTPRequestHandler):
        def log_message(self, *a):
            pass

        def _send(self, body, ctype="application/json"):
            data = body.encode("utf-8") if isinstance(body, str) else body
            self.send_response(200)
            self.send_header("Content-Type", ctype)
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def do_GET(self):
            from urllib.parse import urlparse, parse_qs
            u = urlparse(self.path)
            qs = parse_qs(u.query)
            try:
                if u.path in ("/", "/index.html"):
                    return self._send(EXPLORER_HTML, "text/html; charset=utf-8")
                if u.path == "/api/summary":
                    return self._send(json.dumps(api_summary(con)))
                if u.path == "/api/functions":
                    return self._send(json.dumps(api_functions(con)))
                if u.path == "/api/timeline":
                    return self._send(json.dumps(api_timeline(con, int(qs.get("buckets", ["240"])[0]))))
                if u.path == "/api/deopts":
                    return self._send(json.dumps(api_deopts(con, int(qs.get("limit", ["500"])[0]))))
                if u.path == "/api/function":
                    return self._send(json.dumps(api_function(con, int(qs.get("sym", ["-1"])[0]))))
            except Exception as e:  # noqa: BLE001 — report, don't crash the server
                self.send_response(500)
                self.end_headers()
                self.wfile.write(str(e).encode())
                return
            self.send_response(404)
            self.end_headers()

    srv = http.server.ThreadingHTTPServer(("127.0.0.1", port), H)
    print(f"[jitrec] serving {db_path} at http://localhost:{port}  (Ctrl-C to stop)", file=sys.stderr)
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        print("\n[jitrec] stopped", file=sys.stderr)


EXPLORER_HTML = r"""<!doctype html><html lang=en><head><meta charset=utf-8>
<meta name=viewport content="width=device-width,initial-scale=1"><title>Bliss JIT Explorer</title>
<link rel=preconnect href=https://fonts.googleapis.com><link rel=preconnect href=https://fonts.gstatic.com crossorigin>
<link rel=stylesheet href="https://fonts.googleapis.com/css2?family=IBM+Plex+Mono:wght@400;500;600&family=IBM+Plex+Sans:wght@400;500;600;700&display=swap">
<style>
:root{--bg:#eef1f5;--surface:#fff;--surface-2:#f5f7fa;--ink:#1b2027;--muted:#5c6672;--faint:#8a93a0;--line:#dbe0e8;--accent:#d9541f;
--k-t1:#1f8f86;--k-t2:#2f9e4e;--k-osr:#3f74c9;--k-deopt:#d98324;--k-black:#c23b3b;--k-gc:#7a8699;--k-gcmaj:#8a63b0;
--font-ui:'IBM Plex Sans',system-ui,sans-serif;--font-mono:'IBM Plex Mono',ui-monospace,monospace;--radius:9px;
--shadow:0 1px 2px rgba(20,30,45,.06),0 4px 16px rgba(20,30,45,.05)}
@media(prefers-color-scheme:dark){:root:not([data-theme=light]){--bg:#12151b;--surface:#1a1e26;--surface-2:#21262f;--ink:#e7eaf0;--muted:#9aa4b1;--faint:#6a7480;--line:#2b323d;--accent:#f0743a;
--k-t1:#35b6ab;--k-t2:#4cc26d;--k-osr:#5b93e6;--k-deopt:#e8a13f;--k-black:#e5615f;--k-gc:#8b97aa;--k-gcmaj:#a884cf}}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--ink);font-family:var(--font-ui);line-height:1.5}
.wrap{max-width:1180px;margin:0 auto;padding:28px 22px 80px}
header.m{display:flex;align-items:baseline;gap:14px;border-bottom:2px solid var(--accent);padding-bottom:14px}
header.m h1{font-size:1.6rem;font-weight:700;letter-spacing:-.02em;margin:0}
header.m .e{font-family:var(--font-mono);font-size:.72rem;letter-spacing:.16em;text-transform:uppercase;color:var(--accent)}
header.m .sub{color:var(--muted);font-size:.9rem;margin-left:auto}
.chips{display:flex;flex-wrap:wrap;gap:10px;margin:22px 0}
.chip{background:var(--surface);border:1px solid var(--line);border-radius:var(--radius);padding:9px 14px;min-width:92px;box-shadow:var(--shadow)}
.chip .n{font-family:var(--font-mono);font-size:1.35rem;font-weight:600;font-variant-numeric:tabular-nums}
.chip .l{font-size:.7rem;text-transform:uppercase;letter-spacing:.08em;color:var(--muted);margin-top:2px}
section{margin-top:30px}h2{font-size:1rem;font-weight:600;margin:0 0 10px}h2 .h{font-weight:400;font-size:.82rem;color:var(--faint)}
.panel{background:var(--surface);border:1px solid var(--line);border-radius:var(--radius);box-shadow:var(--shadow);padding:14px 16px}
svg.tl{display:block;width:100%}svg.tl text{font-family:var(--font-mono);fill:var(--muted)}
table{width:100%;border-collapse:collapse;font-size:.87rem}th,td{text-align:right;padding:7px 10px;border-bottom:1px solid var(--line)}
th:first-child,td:first-child{text-align:left}th{font-size:.7rem;text-transform:uppercase;letter-spacing:.06em;color:var(--muted);font-weight:600}
td.name{font-family:var(--font-mono);font-weight:500}td.num{font-variant-numeric:tabular-nums;font-family:var(--font-mono)}
tr.fnrow{cursor:pointer}tr.fnrow:hover td{background:var(--surface-2)}.heatbar{height:7px;border-radius:3px;background:linear-gradient(90deg,var(--k-t1),var(--accent));display:inline-block;vertical-align:middle;margin-left:8px}
input#filter{font:inherit;padding:6px 11px;border:1px solid var(--line);border-radius:7px;background:var(--surface);color:var(--ink);width:220px;margin-bottom:10px}
#backdrop{position:fixed;inset:0;background:rgba(10,14,20,.44);opacity:0;pointer-events:none;transition:.18s;z-index:30}#backdrop.open{opacity:1;pointer-events:auto}
#drawer{position:fixed;top:0;right:0;height:100%;width:min(720px,95vw);background:var(--surface);border-left:1px solid var(--line);box-shadow:-12px 0 40px rgba(10,14,20,.28);z-index:31;transform:translateX(100%);transition:transform .22s;display:flex;flex-direction:column}
#drawer.open{transform:translateX(0)}.dh{padding:18px 20px 12px;border-bottom:1px solid var(--line);display:flex;align-items:center;gap:10px}
.dh h3{font-family:var(--font-mono);font-size:1.1rem;margin:0;word-break:break-all}.dh button{margin-left:auto;background:none;border:1px solid var(--line);color:var(--muted);border-radius:7px;width:30px;height:30px;font-size:1.1rem;cursor:pointer}
.db{overflow-y:auto;padding:16px 20px 40px;flex:1}.db h4{font-size:.72rem;text-transform:uppercase;letter-spacing:.08em;color:var(--muted);margin:18px 0 8px}.db h4:first-child{margin-top:0}
.statline{display:flex;flex-wrap:wrap;gap:7px}.stat{font-family:var(--font-mono);font-size:.8rem;padding:4px 10px;border:1px solid var(--line);border-radius:6px;background:var(--surface-2)}
.tiertabs{display:flex;gap:4px;flex-wrap:wrap;margin-bottom:8px}.tiertab{font:inherit;font-family:var(--font-mono);font-size:.78rem;cursor:pointer;background:var(--surface-2);color:var(--muted);border:1px solid var(--line);border-bottom:none;border-radius:7px 7px 0 0;padding:6px 12px}
.tiertab[aria-pressed=true]{background:var(--accent);color:#fff;border-color:var(--accent);font-weight:600}
.disasm{font-family:var(--font-mono);font-size:.77rem;line-height:1.5;background:var(--surface-2);border:1px solid var(--line);border-radius:8px;padding:11px 13px;overflow-x:auto;white-space:pre;margin:0}
.disasm .c{color:var(--faint)}.disasm .spec{color:var(--accent);font-weight:600}.disasm .op{color:var(--ink)}
.corr{display:flex;gap:10px;overflow-x:auto}.corr .col{flex:1 1 0;min-width:200px}.corr .col h5{margin:0 0 5px;font:600 .7rem/1 var(--font-mono);text-transform:uppercase;letter-spacing:.08em;color:var(--muted)}
.corr pre{margin:0;font-family:var(--font-mono);font-size:.73rem;line-height:1.5;background:var(--surface-2);border:1px solid var(--line);border-radius:8px;padding:10px;white-space:pre;max-height:62vh;overflow:auto;scroll-behavior:smooth}
@media(prefers-reduced-motion:reduce){.corr pre{scroll-behavior:auto}}
.corr .ln{display:block;border-radius:3px;padding:0 3px;margin:0 -3px}.corr .ln[data-bcp]{cursor:pointer}.corr .ln.lit{background:var(--accent);color:#fff}.corr .ln.lit .c,.corr .ln.lit .op,.corr .ln.lit .spec{color:#fff}
.corr-hint{font-size:.78rem;color:var(--faint);margin:2px 0 8px}.empty{color:var(--faint);font-style:italic}
</style></head><body><div class=wrap>
<header class=m><div><div class=e>bliss · jitrec explorer</div><h1>JIT Recording</h1></div><span class=sub id=sub>loading…</span></header>
<div class=chips id=chips></div>
<section><h2>Timeline <span class=h>event density over the run, by kind (bucketed — scales to any size)</span></h2><div class=panel><svg class=tl id=tl></svg></div></section>
<section><h2>Functions <span class=h>JIT activity, hottest first — click to inspect</span></h2><input id=filter placeholder="filter by name…"><div class=panel style=overflow-x:auto><table><thead><tr><th>Function<th>T1<th>T2<th>Deopts<th>OSR<th>Activity</tr></thead><tbody id=fnbody></tbody></table></div></section>
</div>
<div id=backdrop></div><aside id=drawer><div class=dh><h3 id=dt></h3><button id=dc aria-label=Close>×</button></div><div class=db id=dbody></div></aside>
<!--SNAP-->
<script>
const NO_OFF=4294967295, DR={0:"guard",1:"phase-change",2:"blacklist"};
const $=id=>document.getElementById(id);
// In a live server the data comes from the API; a `snapshot` bakes it into
// window.SNAP so the same page renders offline.
const gj=async u=>(window.SNAP&&(u in window.SNAP))?window.SNAP[u]:(await fetch(u)).json();
const esc=s=>String(s).replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
const OPS=/\b(CallNamed|LoadLocal|LoadGlobal|StoreGlobal|StoreLocal|Const|Return|Br|BrIfFalse|BrIfTrue|Go|PushBlock|PopHandler|call|jmp|je|jne|jz|jnz|jg|jge|jl|jle|ret|mov|movabs|cmp|test|add|sub|imul|lea|push|pop|xor|and|or|shl|shr|sar)\b/g;
function tintCode(s){return s.replace(/(^\s*(?:\+[0-9a-f]+|\d+):)/,'<span class=c>$1</span>').replace(OPS,'<span class=op>$1</span>')}
function tintDisasm(t){return t.split("\n").map(l=>{let e=esc(l);if(/^\s*;/.test(l))return`<span class=c>${e}</span>`;const i=e.indexOf("; ");if(i>=0){let c=e.slice(i).replace(/(⇒ speculate [A-Z-]+)/g,'<span class=spec>$1</span>');return tintCode(e.slice(0,i))+`<span class=c>${c}</span>`}return tintCode(e)}).join("\n")}
function tintLisp(t){let s=esc(t);s=s.replace(/\b(defun|lambda|let|let\*|labels|flet|if|cond|when|unless|dotimes|dolist|loop|block|return-from|setq|setf|progn)\b/g,'<span class=op>$1</span>');return s.replace(/(^\(defun\s+)([^\s()]+)/,'$1<span class=spec>$2</span>')}
function bcpForOffset(map,off){const es=map.map((o,b)=>({o,b})).filter(e=>e.o!==NO_OFF).sort((a,b)=>a.o-b.o);let bcp=null;for(const e of es){if(e.o<=off)bcp=e.b;else break}return bcp}
function corrLines(text,native,map){return text.split("\n").map(l=>{const e=tintDisasm(l);let b=null;if(native){const m=l.match(/^\s*\+([0-9a-f]+):/);if(m)b=bcpForOffset(map||[],parseInt(m[1],16))}else{const m=l.match(/^\s*(\d+):/);if(m)b=+m[1]}return`<span class=ln${b!=null?` data-bcp="${b}"`:""}>${e}</span>`}).join("")}
function renderMulti(f){
 const cols=[["T0 · bytecode",corrLines(f.t0,false,null)]];
 if(f.t1)cols.push(["T1 · native x86-64",corrLines(f.t1,true,f.t1map)]);
 if(f.t2)cols.push(["T2 · native x86-64",corrLines(f.t2,true,f.t2map)]);
 const sparse=f.t2map&&f.t2map.some(o=>o===NO_OFF);
 const body=cols.map(([h,c])=>`<div class=col><h5>${h}</h5><pre>${c}</pre></div>`).join("");
 return`<p class=corr-hint>All tiers side by side — hover a line to light up the matching bytecode ↔ native across every pane and scroll them into view.${sparse?" (T2 is optimizer output — correlation is anchored at frame-state points.)":""}</p><div class=corr>${body}</div>`}
function wireCorr(root){const panes=[...root.querySelectorAll(".corr pre")];const all=root.querySelectorAll(".ln[data-bcp]");
 const lit=(b,on)=>all.forEach(el=>{if(el.dataset.bcp===b)el.classList.toggle("lit",on)});
 const sync=(b,own)=>panes.forEach(p=>{if(p===own)return;const t=p.querySelector('.ln[data-bcp="'+b+'"]');if(!t)return;const pr=p.getBoundingClientRect(),tr=t.getBoundingClientRect();if(tr.top>=pr.top&&tr.bottom<=pr.bottom)return;p.scrollTop+=(tr.top-pr.top)-p.clientHeight/2+tr.height/2});
 all.forEach(el=>{el.onmouseenter=()=>{lit(el.dataset.bcp,true);sync(el.dataset.bcp,el.closest("pre"))};el.onmouseleave=()=>lit(el.dataset.bcp,false)})}

let SPAN=[0,1];const ms=ns=>(ns-SPAN[0])/1e6;
async function boot(){
 const s=await gj("/api/summary");SPAN=s.span_ns;
 $("sub").textContent=`${s.total.toLocaleString()} events · ${s.functions} functions · ${(ms(SPAN[1])).toFixed(0)} ms`;
 const k=s.kinds||{};
 $("chips").innerHTML=[["compile","Compiles"],["deopt","Deopts"],["osr","OSR entries"]].map(([kk,l])=>`<div class=chip><div class=n>${(k[kk]||0).toLocaleString()}</div><div class=l>${l}</div></div>`).join("")
  +`<div class=chip><div class=n>${((k["gc-minor"]||0)+(k["gc-major"]||0)).toLocaleString()}</div><div class=l>GC pauses</div></div>`
  +`<div class=chip><div class=n>${(s.gc_pause_us/1000).toLocaleString(undefined,{maximumFractionDigits:1})} ms</div><div class=l>GC pause total</div></div>`;
 drawTimeline(await gj("/api/timeline?buckets=240"));
 window.FNS=(await gj("/api/functions")).functions;renderFns("");
}
const KCOL={compile:"--k-t2",deopt:"--k-deopt",osr:"--k-osr","gc-minor":"--k-gc","gc-major":"--k-gcmaj"};
function cv(n){return getComputedStyle(document.documentElement).getPropertyValue(n).trim()}
function drawTimeline(d){
 const kinds=[...new Set(d.rows.map(r=>r.kind))];const H=26*kinds.length+26,W=1120,padL=78;
 const max={};d.rows.forEach(r=>{max[r.kind]=Math.max(max[r.kind]||0,r.n)});
 let s="";kinds.forEach((k,i)=>{const y=i*26+4;s+=`<text x="${padL-8}" y="${y+15}" text-anchor=end font-size=11>${k}</text>`;
  d.rows.filter(r=>r.kind===k).forEach(r=>{const x=padL+r.bucket/d.buckets*(W-padL-10);const h=Math.max(1,r.n/max[k]*20);s+=`<rect x="${x.toFixed(1)}" y="${y+22-h}" width="${Math.max(1,(W-padL-10)/d.buckets-.5).toFixed(2)}" height="${h.toFixed(1)}" fill="${cv(KCOL[k]||'--muted')}"></rect>`})});
 const ay=kinds.length*26+2;for(let i=0;i<=6;i++){const x=padL+i/6*(W-padL-10);s+=`<line x1="${x}" y1=2 x2="${x}" y2="${ay}" stroke="${cv('--line')}"></line><text x="${x}" y="${ay+15}" text-anchor=middle font-size=10>${(i/6*ms(SPAN[1])).toFixed(0)} ms</text>`}
 const svg=$("tl");svg.setAttribute("viewBox",`0 0 ${W} ${H}`);svg.setAttribute("height",H);svg.innerHTML=s}
function renderFns(flt){
 const rows=window.FNS.filter(f=>!flt||f.name.toLowerCase().includes(flt));
 const mx=rows.reduce((m,r)=>Math.max(m,r.t1+r.t2+r.deopt+r.osr),1);
 $("fnbody").innerHTML=rows.slice(0,500).map(r=>{const tot=r.t1+r.t2+r.deopt+r.osr;return`<tr class=fnrow data-sym="${r.sym}"><td class=name>${esc(r.name)}</td><td class=num>${r.t1||'·'}</td><td class=num>${r.t2||'·'}</td><td class=num style="color:${r.deopt?'var(--k-black)':'inherit'}">${r.deopt||'·'}</td><td class=num>${r.osr||'·'}</td><td><span class=num style=color:var(--muted)>${tot}</span><span class=heatbar style="width:${Math.round(tot/mx*110)}px"></span></td></tr>`}).join("")
  +(rows.length>500?`<tr><td colspan=6 class=empty>… ${rows.length-500} more (filter to narrow)</td></tr>`:"");
 $("fnbody").querySelectorAll("tr.fnrow").forEach(tr=>tr.onclick=()=>openFn(+tr.dataset.sym,tr.querySelector(".name").textContent));
}
$("filter").oninput=e=>renderFns(e.target.value.toLowerCase());

async function openFn(sym,name){
 $("dt").textContent=name;$("dbody").innerHTML="<p class=empty>loading…</p>";
 $("backdrop").classList.add("open");$("drawer").classList.add("open");
 const f=await gj("/api/function?sym="+sym);
 const fr=window.FNS.find(x=>x.sym===sym)||{t1:0,t2:0,deopt:0,osr:0};
 const stats=`<div class=statline><span class=stat>T1 <b>${fr.t1}</b></span><span class=stat>T2 <b>${fr.t2}</b></span><span class=stat>deopts <b>${fr.deopt}</b></span><span class=stat>OSR <b>${fr.osr}</b></span></div>`;
 const hasMulti=f.t0&&(f.t1||f.t2);const tiers=[];if(hasMulti)tiers.push(["all","compare tiers"]);
 [["src","source"],["t0","T0 · bytecode"],["t1","T1 · native"],["t2","T2 · native"]].forEach(t=>{if(f[t[0]])tiers.push(t)});
 const rt=k=>k==="all"?renderMulti(f):k==="src"?`<pre class=disasm>${tintLisp(f[k])}</pre>`:`<pre class=disasm>${tintDisasm(f[k])}</pre>`;
 const def=(tiers[0]||[])[0];
 const tabs=tiers.map(([k,l])=>`<button class=tiertab data-tier="${k}" aria-pressed="${k===def}">${l}</button>`).join("");
 $("dbody").innerHTML=`<h4>JIT activity</h4>${stats}<h4>Representations</h4>${tiers.length?`<div class=tiertabs>${tabs}</div><div id=pane></div>`:'<p class=empty>No disassembly.</p>'}`;
 const pane=$("pane");const paint=k=>{pane.innerHTML=rt(k);if(k==="all")wireCorr(pane)};
 if(tiers.length)paint(def);
 document.querySelectorAll(".tiertab").forEach(b=>b.onclick=()=>{document.querySelectorAll(".tiertab").forEach(x=>x.setAttribute("aria-pressed","false"));b.setAttribute("aria-pressed","true");paint(b.dataset.tier)});
}
function closeDrawer(){$("backdrop").classList.remove("open");$("drawer").classList.remove("open")}
$("dc").onclick=closeDrawer;$("backdrop").onclick=closeDrawer;document.addEventListener("keydown",e=>{if(e.key==="Escape")closeDrawer()});
boot();
</script></body></html>"""


def snapshot(db_path, out_path):
    """Bake a DB's query responses into a self-contained explorer page — the same
    UI as `serve`, but rendering offline (for sharing a specific recording, or a
    demo). Re-embeds everything, so it is for a bounded run, not a giant one."""
    con = sqlite3.connect(db_path)
    snap = {
        "/api/summary": api_summary(con),
        "/api/functions": api_functions(con),
        "/api/timeline?buckets=240": api_timeline(con, 240),
    }
    for f in snap["/api/functions"]["functions"]:
        snap[f"/api/function?sym={f['sym']}"] = api_function(con, f["sym"])
    con.close()
    html = EXPLORER_HTML.replace(
        "<!--SNAP-->", "<script>window.SNAP=" + json.dumps(snap) + ";</script>"
    )
    with open(out_path, "w", encoding="utf-8") as fh:
        fh.write(html)
    print(f"[jitrec] wrote self-contained snapshot {out_path} "
          f"({len(snap['/api/functions']['functions'])} functions)", file=sys.stderr)


def main():
    a = sys.argv[1:]
    if len(a) >= 3 and a[0] == "ingest":
        ingest(a[1], a[2])
    elif len(a) >= 2 and a[0] == "serve":
        serve(a[1], int(a[2]) if len(a) > 2 else 8765)
    elif len(a) >= 3 and a[0] == "snapshot":
        snapshot(a[1], a[2])
    else:
        print(__doc__)
        print("usage: jitrec.py ingest   <run.ndjson|-> <run.db>\n"
              "       jitrec.py serve    <run.db> [port]\n"
              "       jitrec.py snapshot <run.db> <out.html>", file=sys.stderr)
        sys.exit(2)


if __name__ == "__main__":
    main()
