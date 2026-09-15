/* traciium trace viewer — vanilla JS, no dependencies. */
"use strict";

const $ = (sel) => document.querySelector(sel);
const state = {
  meta: null,          // {format, format_path, files}
  fileIdx: 0,
  runId: null,
  run: null,           // full run incl. events
  view: "stream",
  query: "",
  mutedTypes: new Set(),
  focus: "events",
  pendingG: false,
};

/* ---------------- utils ---------------- */

function el(tag, attrs = {}, ...children) {
  const n = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k === "class") n.className = v;
    else if (k === "html") n.innerHTML = v;
    else if (k.startsWith("on")) n.addEventListener(k.slice(2), v);
    else if (v !== null && v !== undefined) n.setAttribute(k, v);
  }
  for (const c of children) {
    if (c == null) continue;
    n.append(c);
  }
  return n;
}

function fmtDur(sec) {
  if (sec == null) return "–";
  if (sec < 0.001) return `${(sec * 1e6) | 0}µs`;
  if (sec < 1) return `${(sec * 1000).toFixed(0)}ms`;
  if (sec < 60) return `${sec.toFixed(2)}s`;
  if (sec < 3600) {
    const m = Math.floor(sec / 60), s = sec % 60;
    return `${m}m ${s.toFixed(0)}s`;
  }
  const h = Math.floor(sec / 3600), m = Math.round((sec % 3600) / 60);
  return `${h}h ${m}m`;
}

function fmtTs(ts) {
  if (ts == null) return "–";
  if (Math.abs(ts) > 1e12) ts = ts / 1000; // unix ms -> s
  const d = new Date(ts * 1000);
  return d.toISOString().replace("T", " ").replace(/(\.\d{3})\d*Z$/, "$1");
}

function fmtNum(n) {
  if (n == null) return "–";
  return n.toLocaleString("en-US");
}

function fmtCompact(n) {
  if (n == null) return "–";
  if (n >= 1e6) return (n / 1e6).toFixed(n >= 1e7 ? 0 : 1) + "M";
  if (n >= 1e3) return (n / 1e3).toFixed(n >= 1e4 ? 0 : 1) + "k";
  return String(n);
}

/* timestamp relative to the run start, e.g. +2.34s / +1m12s */
function fmtRel(ts, t0) {
  if (ts == null || t0 == null) return "";
  const d = ts - t0;
  const sign = d < 0 ? "−" : "+";
  return sign + fmtDur(Math.abs(d));
}

function esc(s) {
  return String(s).replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}

function getPath(obj, path) {
  return path.split(".").reduce((v, k) => (v == null ? undefined : v[k]), obj);
}

/* ---------------- format spec helpers ---------------- */

function spec() { return state.meta.format; }

function eventSpec(ty) {
  return (spec().events || {})[ty] || {};
}

function eventTone(ty) {
  const t = eventSpec(ty).tone;
  if (t) return t;
  const palette = ["violet", "sky", "emerald", "amber", "cyan", "pink", "lime", "orange", "blue", "rose"];
  let h = 0;
  for (const b of String(ty)) h = (h * 31 + b.charCodeAt(0)) >>> 0;
  return palette[h % palette.length];
}

function eventLabel(ty) {
  const l = eventSpec(ty).label;
  if (l) return l;
  return String(ty).replace(/_/g, " ");
}

function eventIcon(ty) {
  return eventSpec(ty).icon || "●";
}

function isIgnored(field) {
  return (spec().defaults?.ignore || []).includes(field);
}

function toneColor(tone) {
  return `var(--tone-${tone}, ${tone})`;
}

function valueStyle(field, value) {
  const vs = spec().value_styles?.[field];
  if (!vs) return null;
  const key = typeof value === "boolean" ? String(value) : String(value);
  return vs[key] || null;
}

function conditionalStyle(record) {
  for (const cond of spec().conditional_styles || []) {
    const ok = Object.entries(cond.when || {}).every(([k, v]) => {
      const got = getPath(record, k);
      return JSON.stringify(got) === JSON.stringify(v);
    });
    if (ok) return cond.style || {};
  }
  return {};
}

/* ---------------- syntax highlighting ---------------- */

const LANGS = {
  bash: [
    [/#.*$/m, "com"],
    [/(^|\s)(if|then|else|elif|fi|for|while|do|done|case|esac|in|function|return|export|local|source)(\s|$)/gm, "kw"],
    [/("(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*')/, "str"],
    [/(\$\{?[\w@#?*]+\}?)/, "var"],
    [/(^|\s|;|&&|\|\|)(sudo|apt|apt-get|pip|pip3|python|python3|cargo|git|cat|ls|cd|cp|mv|rm|mkdir|touch|echo|grep|sed|awk|find|curl|wget|chmod|chown|make|cmake|head|tail|wc|sort|uniq|tee|xargs|which|env|export|pytest|just|uv)\b/g, "cmd"],
    [/(--?[\w-]+)/, "op"],
    [/(\|\||&&|[|><&;])/, "op"],
    [/\b(\d+(?:\.\d+)?)\b/, "num"],
  ],
  python: [
    [/(#[^\n]*)/, "com"],
    [/("""[\s\S]*?"""|'''[\s\S]*?''')/, "str"],
    [/("(?:[^"\\\n]|\\.)*"|'(?:[^'\\\n]|\\.)*')/, "str"],
    [/\b(def|class|return|if|elif|else|for|while|import|from|as|with|try|except|finally|raise|assert|yield|lambda|global|nonlocal|pass|break|continue|and|or|not|in|is|None|True|False|async|await|del)\b/g, "kw"],
    [/\b(print|len|range|open|str|int|float|list|dict|set|tuple|enumerate|zip|map|filter|isinstance|super|self)\b/g, "fn"],
    [/(@[\w.]+)/, "var"],
    [/\b(\d+(?:\.\d+)?)\b/, "num"],
    [/([+\-*/%=<>!&|^~]=?|:=)/, "op"],
  ],
  json: [
    [/(\/\/[^\n]*)/, "com"],
    [/("(?:[^"\\]|\\.)*")(\s*:)?/, "key"],
    [/("(?:[^"\\]|\\.)*")/, "str"],
    [/\b(true|false|null)\b/g, "bool"],
    [/-?\b\d+(?:\.\d+)?(?:[eE][+-]?\d+)?\b/, "num"],
    [/([{}\[\],:])/, "op"],
  ],
  yaml: [
    [/(#[^\n]*)/, "com"],
    [/(\n\s*- )/, "op"],
    [/([ \t]*[\w.\-]+)(:)/g, "key"],
    [/("(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*')/, "str"],
    [/\b(true|false|null|yes|no|on|off)\b/g, "bool"],
    [/\b(\d+(?:\.\d+)?)\b/, "num"],
  ],
};

function highlight(code, lang) {
  const rules = LANGS[lang];
  if (!rules) return esc(code);
  let i = 0, out = "", text = String(code);
  const n = text.length;
  while (i < n) {
    let best = null;
    for (const [re, cls] of rules) {
      re.lastIndex = 0;
      const m = re.exec(text.slice(i));
      if (m && m[0] && (best == null || m.index < best.index)) {
        best = { index: m.index, len: m[0].length, cls, m };
      }
    }
    if (!best || best.index > 0) {
      const stop = best ? best.index : n - i;
      out += esc(text.slice(i, i + stop));
      i += stop;
    } else {
      let cls = best.cls;
      // json: "key": vs plain string
      if (lang === "json" && cls === "key" && best.m[2] === undefined) cls = "str";
      out += `<span class="tk-${cls}">${esc(best.m[0])}</span>`;
      i += best.len;
    }
  }
  return out;
}

function guessLang(text) {
  const t = text.trimStart();
  if (t.startsWith("{") || t.startsWith("[")) return "json";
  if (/^(def |class |import |from |print\()/m.test(t)) return "python";
  if (/^[ \t]*[\w-]+:(\s|$)/m.test(t) && !t.includes(";") && !t.includes("{")) return "yaml";
  return "bash";
}

/* code block with truncation + "show more" */
function codeBlock(text, lang, opts = {}) {
  const max = opts.max ?? spec().defaults?.max_text ?? 20000;
  const full = String(text ?? "");
  const pre = el("pre", { class: "code" + (opts.wrap ? " wrap" : "") });
  const shown = full.length > max ? full.slice(0, max) : full;
  const render = (txt) => { pre.innerHTML = highlight(txt, lang) ; pre.scrollTop = 0; };
  render(shown);
  if (full.length > max) {
    const more = el("span", { class: "more" }, `show all ${fmtNum(full.length)} chars`);
    more.addEventListener("click", (e) => { e.stopPropagation(); render(full); more.remove(); });
    pre.append(more);
  }
  return pre;
}

/* ---------------- markdown (minimal, safe) ---------------- */

function md(text) {
  const lines = String(text ?? "").split("\n");
  let html = "", inCode = false, codeBuf = [], codeLang = "", listType = null;
  const closeList = () => { if (listType) { html += `</${listType}>`; listType = null; } };
  const inline = (s) => {
    s = esc(s);
    s = s.replace(/`([^`]+)`/g, "<code>$1</code>");
    s = s.replace(/\*\*([^*]+)\*\*/g, "<b>$1</b>");
    s = s.replace(/\*([^*]+)\*/g, "<i>$1</i>");
    s = s.replace(/\[([^\]]+)\]\(([^)]+)\)/g, '<a href="$2" target="_blank" rel="noreferrer">$1</a>');
    return s;
  };
  for (const raw of lines) {
    if (raw.trimStart().startsWith("```")) {
      if (inCode) {
        html += `<pre class="code">${highlight(codeBuf.join("\n"), codeLang || guessLang(codeBuf.join("\n")))}</pre>`;
        inCode = false; codeBuf = [];
      } else {
        closeList();
        inCode = true;
        codeLang = raw.trim().slice(3).trim();
      }
      continue;
    }
    if (inCode) { codeBuf.push(raw); continue; }
    const line = raw;
    const h = line.match(/^(#{1,4})\s+(.*)/);
    if (h) { closeList(); html += `<h${h[1].length}>${inline(h[2])}</h${h[1].length}>`; continue; }
    if (/^\s*([-*])\s+/.test(line)) {
      if (listType !== "ul") { closeList(); html += "<ul>"; listType = "ul"; }
      html += `<li>${inline(line.replace(/^\s*[-*]\s+/, ""))}</li>`; continue;
    }
    if (/^\s*\d+\.\s+/.test(line)) {
      if (listType !== "ol") { closeList(); html += "<ol>"; listType = "ol"; }
      html += `<li>${inline(line.replace(/^\s*\d+\.\s+/, ""))}</li>`; continue;
    }
    if (/^\s*>\s?/.test(line)) { closeList(); html += `<blockquote>${inline(line.replace(/^\s*>\s?/, ""))}</blockquote>`; continue; }
    if (/^\s*(---+|\*\*\*+)\s*$/.test(line)) { closeList(); html += "<hr>"; continue; }
    if (line.trim() === "") { closeList(); continue; }
    closeList();
    html += `<p>${inline(line)}</p>`;
  }
  if (inCode) html += `<pre class="code">${esc(codeBuf.join("\n"))}</pre>`;
  closeList();
  return html;
}

/* ---------------- body field renderers ---------------- */

function renderValue(field, v, lang) {
  if (v == null) return null;
  if (typeof v === "boolean" || typeof v === "number") {
    const vs = valueStyle(field, v);
    const label = vs?.label ?? String(v);
    const chip = el("span", { class: "vchip" }, (vs?.prefix || "") + label);
    if (vs?.tone) chip.style.color = toneColor(vs.tone);
    return chip;
  }
  if (typeof v === "string") {
    return codeBlock(v, lang || guessLang(v), { wrap: true });
  }
  return codeBlock(JSON.stringify(v, null, 2), "json");
}

const RENDERERS = {
  markdown: (v) => {
    const d = el("div", { class: "md" });
    d.innerHTML = md(typeof v === "string" ? v : JSON.stringify(v, null, 2));
    return d;
  },
  code: (v, _, opts) => codeBlock(typeof v === "string" ? v : JSON.stringify(v, null, 2), opts.lang || guessLang(String(v))),
  output: (v) => codeBlock(String(v ?? ""), null, { wrap: true }),
  json: (v) => codeBlock(JSON.stringify(v, null, 2), "json"),
  text: (v, field, opts) => {
    if (typeof v !== "string") return renderValue(field, v, opts.lang);
    const vs = valueStyle(field, v);
    const d = el("div", { class: "plaintext" });
    d.textContent = (vs?.prefix || "") + (vs?.label ?? v);
    if (vs?.tone) d.style.color = toneColor(vs.tone);
    return d;
  },
  kv: (v) => {
    const grid = el("div", { class: "kv" });
    const entries = typeof v === "object" && v !== null ? Object.entries(v) : [];
    for (const [k, val] of entries) {
      grid.append(el("div", { class: "k" }, k));
      grid.append(el("div", { class: "v" }, typeof val === "object" ? JSON.stringify(val) : String(val)));
    }
    return entries.length ? grid : codeBlock(JSON.stringify(v, null, 2), "json");
  },
  tokens: (v) => {
    const row = el("div", { class: "chips" });
    const add = (label, n) => {
      if (!n) return; // skip zero/missing token counts — pure noise
      row.append(el("span", { class: "ev-badge" }, label + " ", el("b", {}, fmtCompact(n))));
    };
    add("prompt", v.prompt);
    add("completion", v.completion);
    if (v.reasoning) add("reasoning", v.reasoning);
    if (v.cached_prompt) add("cached", v.cached_prompt);
    add("total", v.total);
    return row.childNodes.length ? row : null;
  },
  messages: (v) => {
    const wrap = el("div");
    for (const m of Array.isArray(v) ? v : []) {
      const box = el("div", { class: "msg", "data-role": m.role || "unknown" });
      box.append(el("span", { class: "msg-role" }, m.role || "unknown"));
      const content = m.content ?? "";
      const c = el("div", { class: "msg-content" });
      if (typeof content === "string") {
        c.innerHTML = md(content.slice(0, 12000));
      } else {
        c.append(codeBlock(JSON.stringify(content, null, 2), "json"));
      }
      box.append(c);
      wrap.append(box);
    }
    return wrap;
  },
  /* one-line summary per call; the full args live in the tool_call cards
     (click a row to jump there) — avoids rendering the same code twice */
  tool_calls_brief: (v) => {
    const wrap = el("div");
    for (const call of Array.isArray(v) ? v : []) {
      const args = call.arguments ?? {};
      let preview = "";
      if (typeof args === "string") preview = args;
      else {
        const pk = Object.keys(args).find((k) => PRIMARY_ARG_KEYS.includes(k));
        preview = pk ? String(args[pk]) : JSON.stringify(args);
      }
      preview = preview.replace(/\s+/g, " ").slice(0, 110);
      const row = el("div", { class: "tcall-row", title: "jump to tool call" });
      row.append(el("span", { class: "tcall-name" }, call.name || "?"));
      row.append(el("span", { class: "tcall-args" }, preview));
      row.append(el("span", { class: "tcall-jump" }, "↓"));
      if (call.id) {
        row.addEventListener("click", () => {
          const target = document.querySelector(`[data-call-id="${CSS.escape(call.id)}"]`);
          if (target) {
            target.scrollIntoView({ behavior: "smooth", block: "center" });
            target.classList.add("hl");
            setTimeout(() => target.classList.remove("hl"), 1600);
          }
        });
      }
      wrap.append(row);
    }
    return wrap;
  },
  tool_calls: (v) => {
    const wrap = el("div");
    for (const call of Array.isArray(v) ? v : []) {
      const box = el("div", { class: "tcall" });
      box.append(el("div", { class: "tcall-head" }, call.name || "?", el("span", { class: "arrow" }, "  ·  " + (call.id || ""))));
      const args = call.arguments ?? {};
      box.append(...toolArgsContent(args));
      wrap.append(box);
    }
    return wrap;
  },
  tool_args: (v) => {
    const wrap = el("div");
    wrap.append(...toolArgsContent(v));
    return wrap;
  },
  list: (v) => {
    const wrap = el("div", { class: "chips" });
    for (const item of Array.isArray(v) ? v : []) {
      wrap.append(el("span", { class: "vchip" }, typeof item === "string" ? item : JSON.stringify(item)));
    }
    return wrap;
  },
};

/* smart tool-argument display: show the "primary" payload as highlighted
   code (command/script/code/query…), everything else as key–value rows. */
const PRIMARY_ARG_KEYS = ["command", "cmd", "code", "script", "query", "pattern", "path", "file", "content", "text"];
function toolArgsContent(args) {
  const out = [];
  if (typeof args === "string") {
    try { args = JSON.parse(args); } catch { return [codeBlock(args, "bash", { wrap: true })]; }
  }
  if (args === null || typeof args !== "object") {
    return [codeBlock(JSON.stringify(args), "json")];
  }
  const entries = Object.entries(args);
  const primaryKeys = entries.filter(([k]) => PRIMARY_ARG_KEYS.includes(k));
  const rest = entries.filter(([k]) => !PRIMARY_ARG_KEYS.includes(k));
  for (const [k, v] of primaryKeys) {
    if (typeof v === "string" && v.trim()) {
      out.push(codeBlock(v, guessLang(v), { wrap: true }));
    } else {
      const grid = el("div", { class: "kv" });
      grid.append(el("div", { class: "k" }, k), el("div", { class: "v" }, JSON.stringify(v)));
      out.push(grid);
    }
  }
  if (rest.length) {
    const grid = el("div", { class: "kv" });
    for (const [k, v] of rest) {
      grid.append(el("div", { class: "k" }, k));
      grid.append(el("div", { class: "v" }, typeof v === "object" ? JSON.stringify(v) : String(v)));
    }
    out.push(grid);
  }
  return out.length ? out : [codeBlock("{}", "json")];
}

/* ---------------- stream rendering ---------------- */

function cardId(ev, i) {
  const seq = getPath(ev, spec().trace.sequence_field || "sequence");
  return seq != null ? `ev-${seq}` : `ev-i${i}`;
}

function renderStream(run) {
  const stream = $("#stream");
  stream.innerHTML = "";
  const traceKeys = spec().trace;
  const typeField = traceKeys.type_field;
  const tsField = traceKeys.timestamp_field;
  const t0 = run.t_min;

  // index tool results by call_id for pairing
  const callNames = new Map();
  for (const ev of run.events) {
    if (getPath(ev, typeField) === "tool_call" && ev.call_id) {
      callNames.set(ev.call_id, ev.name || ev.call_id);
    }
  }

  run.events.forEach((ev, i) => {
    const ty = String(getPath(ev, typeField) ?? "event");
    const es = eventSpec(ty);
    const cond = conditionalStyle(ev);
    const tone = cond.tone || es.tone || eventTone(ty);
    const compact = es.compact || false;
    const slim = es.slim || false;

    const card = el("div", {
      class: `ev${compact ? " compact" : ""}${slim ? " slim" : ""}`,
      id: cardId(ev, i),
      "data-i": i,
      "data-type": ty,
    });
    if (ev.call_id) card.setAttribute("data-call-id", ev.call_id);
    card.style.setProperty("--c", toneColor(tone));
    if (cond.weight === "bold" || es.weight === "bold") card.style.borderLeftWidth = "5px";
    if (es.terminal) card.classList.add("terminal");

    // header
    const head = el("div", { class: "ev-head", tabindex: "0", role: "button" });
    head.append(el("span", { class: "ev-icon" }, es.icon || eventIcon(ty)));
    head.append(el("span", { class: "ev-label" }, (cond.label_prefix || "") + eventLabel(ty)));

    const titleFrom = es.title_from;
    let title = null;
    if (titleFrom) {
      const tv = getPath(ev, titleFrom);
      if (tv != null) title = String(tv);
    }
    if (ty === "tool_result" && ev.call_id && callNames.has(ev.call_id)) {
      title = `${callNames.get(ev.call_id)} → result`;
    }
    if (title) head.append(el("span", { class: "ev-title" }, title));

    // badges: header_fields + event badges (deduped, zero-noise skipped)
    const badges = el("span", { class: "ev-badges" });
    const badgeFields = [...new Set([...(spec().defaults?.header_fields || []), ...(es.badges || [])])];
    for (const bf of badgeFields) {
      const bv = getPath(ev, bf);
      if (bv == null || bv === "" || typeof bv === "object") continue;
      if (isIgnored(bf)) continue;
      if (bv === false && !valueStyle(bf, bv)) continue; // unstyled `false` = noise
      // humanize millisecond durations
      let shown = String(bv);
      let name = bf.replace(/_/g, " ");
      if (typeof bv === "number" && /_ms$/.test(bf)) {
        shown = fmtDur(bv / 1000);
        name = bf.replace(/_ms$/, "").replace(/_/g, " ");
      }
      if (typeof bv === "number" && (bf.includes("token") || /(^|_)(total|count)$/.test(bf))) {
        shown = fmtCompact(bv);
      }
      const vs = valueStyle(bf, bv);
      const b = el("span", { class: "ev-badge" }, name + " ");
      const label = vs?.label ?? shown;
      b.append(el("b", { style: vs?.tone ? `color:${toneColor(vs.tone)}` : "" }, (vs?.prefix || "") + label));
      badges.append(b);
    }
    if (ev.usage && !badgeFields.includes("usage")) {
      const tk = RENDERERS.tokens(ev.usage);
      if (tk) badges.append(...tk.childNodes);
    }
    head.append(badges);

    // relative-to-run-start time in the header; absolute on hover
    const ts = getPath(ev, tsField);
    if (ts != null) {
      const num = Number(ts);
      head.append(el("span", { class: "ev-time", title: fmtTs(num) }, fmtRel(num, t0)));
    }
    head.append(el("span", { class: "ev-caret" }, "▶"));

    // body
    const body = el("div", { class: "ev-body" });
    const bodyMap = es.body || {};
    const covered = new Set([...Object.keys(bodyMap), typeField, tsField, ...(spec().defaults?.ignore || [])]);
    for (const [field, kind] of Object.entries(bodyMap)) {
      if (isIgnored(field)) continue;
      const v = getPath(ev, field);
      if (v == null || v === "" || (Array.isArray(v) && !v.length)) continue;
      const fn = RENDERERS[kind] || RENDERERS.text;
      const opts = { lang: es[`${field}_lang`] };
      const fwrap = el("div", { class: "ev-field" });
      if (Object.keys(bodyMap).length > 1 && kind !== "messages") {
        fwrap.append(el("div", { class: "ev-field-label" }, field));
      }
      const rendered = fn(v, field, opts);
      if (rendered) fwrap.append(rendered);
      body.append(fwrap);
    }
    // fallback kv for unlisted fields; skip empty values, cap the noise
    const displayed = new Set([...(titleFrom ? [titleFrom] : []), ...badgeFields]);
    const leftovers = Object.keys(ev).filter((k) => {
      if (covered.has(k) || displayed.has(k)) return false;
      const v = ev[k];
      if (v == null || v === "") return false;
      if (typeof v === "object" && !Array.isArray(v) && !Object.keys(v).length) return false;
      if (Array.isArray(v) && !v.length) return false;
      return true;
    });
    if (leftovers.length && !compact && !slim) {
      const CAP = 8;
      const grid = el("div", { class: "kv" });
      const addRows = (keys) => {
        for (const k of keys) {
          const v = ev[k];
          grid.append(el("div", { class: "k" }, k));
          grid.append(el("div", { class: "v" }, typeof v === "object" ? JSON.stringify(v) : String(v)));
        }
      };
      addRows(leftovers.slice(0, CAP));
      const fwrap = el("div", { class: "ev-field" });
      fwrap.append(grid);
      if (leftovers.length > CAP) {
        const rest = leftovers.slice(CAP);
        const more = el("div", { class: "kv-more" }, `… ${rest.length} more fields`);
        more.addEventListener("click", (e) => { e.stopPropagation(); addRows(rest); more.remove(); });
        fwrap.append(more);
      }
      body.append(fwrap);
    }

    const open = es.open ?? spec().defaults?.open ?? false;
    if (open) card.classList.add("open");
    head.setAttribute("aria-expanded", open ? "true" : "false");

    const toggle = () => {
      const expanded = card.classList.toggle("open");
      head.setAttribute("aria-expanded", expanded ? "true" : "false");
    };
    head.addEventListener("click", toggle);
    head.addEventListener("keydown", (e) => {
      if (e.key === "Enter" || e.key === " ") { e.preventDefault(); e.stopPropagation(); toggle(); }
    });
    card.append(head, body);
    stream.append(card);
  });
  applyFilters();
}
/* ---------------- timeline ---------------- */

function renderTimeline(run) {
  const tl = $("#timeline");
  const wrap = $("#timeline-wrap");
  tl.innerHTML = "";
  const span = run.t_max - run.t_min;
  if (span <= 0 || !run.spans.length) {
    wrap.style.display = "none";
    return;
  }
  wrap.style.display = "";

  const pct = (t) => Math.max(0, Math.min(100, ((t - run.t_min) / span) * 100));

  // two lanes: LLM turns (0) and tools/bus (1); gaps hatch across both
  const laneOf = (sp) => (/llm|response|request/.test(sp.ty) ? 0 : 1);
  const hasLlm = run.spans.some((s) => s.kind !== "gap" && laneOf(s) === 0);
  const lanes = hasLlm ? ["turns", "tools"] : ["activity"];

  for (const name of lanes) {
    const lane = el("div", { class: "tl-lane" });
    lane.append(el("span", { class: "lane-name" }, name));
    tl.append(lane);
  }
  const laneEls = [...tl.querySelectorAll(".tl-lane")];

  for (const sp of run.spans) {
    if (sp.kind === "gap") {
      for (const lane of laneEls) {
        const g = el("div", { class: "tl-gap", title: sp.label });
        g.style.left = pct(sp.start) + "%";
        g.style.width = (pct(sp.end) - pct(sp.start)) + "%";
        lane.append(g);
      }
      continue;
    }
    const lane = laneEls[laneOf(sp)] || laneEls[0];
    const node = el("div", {
      class: sp.kind === "point" ? "tl-point" : "tl-span",
      title: `${sp.label}  ·  ${fmtDur(sp.end - sp.start)}`,
      style: `background:${toneColor(sp.tone)}`,
    });
    node.style.left = sp.kind === "point" ? `calc(${pct(sp.start)}% - 1px)` : pct(sp.start) + "%";
    if (sp.kind !== "point") {
      node.style.width = Math.max(pct(sp.end) - pct(sp.start), 0.12) + "%";
    }
    if (sp.seq != null) node.addEventListener("click", () => jumpToSeq(sp.seq));
    lane.append(node);
  }

  // relative-time axis
  const axis = el("div", { class: "tl-axis" });
  for (const f of [0, 0.25, 0.5, 0.75, 1]) {
    const tick = el("span", { class: "tick", style: `left:${f * 100}%` }, "+" + fmtDur(span * f));
    axis.append(tick);
  }
  tl.append(axis);
}

function jumpToSeq(seq) {
  setView("stream");
  const target = document.getElementById(`ev-${seq}`);
  if (!target) return;
  target.scrollIntoView({ behavior: "smooth", block: "center" });
  target.classList.add("hl");
  setTimeout(() => target.classList.remove("hl"), 1600);
}

/* ---------------- run header ---------------- */

function renderRunHead(run) {
  $("#run-id").textContent = run.id === "all" ? state.meta.files[state.fileIdx].name : run.id;
  const chips = $("#run-chips");
  chips.innerHTML = "";
  const s = run.stats;

  const chip = (label, value, tone) => {
    const c = el("span", { class: "chip" }, label + " ");
    c.append(el("b", {}, value));
    if (tone) { c.style.borderColor = toneColor(tone); c.style.color = toneColor(tone); }
    return c;
  };

  if (s.success != null) {
    const ok = s.success;
    const c = chip("outcome", ok ? "PASS" : "FAIL", ok ? "emerald" : "red");
    c.classList.add("outcome");
    chips.append(c);
  } else if (s.outcome_label != null) {
    const c = chip("outcome", s.outcome_label, "amber");
    c.classList.add("outcome");
    chips.append(c);
  }
  if (s.duration_s > 0.001) chips.append(chip("wall", fmtDur(s.duration_s)));
  chips.append(chip("events", fmtNum(s.events)));
  if (s.tool_calls) chips.append(chip("tool calls", fmtNum(s.tool_calls)));
  if (s.tool_errors) chips.append(chip("tool errors", fmtNum(s.tool_errors), "red"));
  if (s.tokens_prompt || s.tokens_completion) {
    chips.append(chip("tokens", `${fmtCompact(s.tokens_prompt)}p · ${fmtCompact(s.tokens_completion)}c`));
  }
  if (s.llm_time_s > 0 || s.tool_time_s > 0) {
    const c = el("span", { class: "chip" }, "time ");
    const bar = el("span", { class: "timebar", title: `llm ${fmtDur(s.llm_time_s)} · tools ${fmtDur(s.tool_time_s)}` });
    const tot = s.llm_time_s + s.tool_time_s || 1;
    bar.append(el("span", { class: "seg-llm", style: `width:${(s.llm_time_s / tot) * 100}%` }));
    bar.append(el("span", { class: "seg-tool", style: `width:${(s.tool_time_s / tot) * 100}%` }));
    c.append(bar);
    c.append(el("b", {}, `llm ${fmtDur(s.llm_time_s)} · tools ${fmtDur(s.tool_time_s)}`));
    chips.append(c);
  }
}

/* ---------------- sidebar ---------------- */

function renderSidebar() {
  const list = $("#filelist");
  list.innerHTML = "";
  $("#fmt-path").textContent = state.meta.format_path.split("/").pop();
  $("#fmt-path").title = state.meta.format_path;
  state.meta.files.forEach((f, fi) => {
    const group = el("div", { class: "file-group" });
    group.append(el("div", { class: "file-name", title: f.path },
      el("span", { class: "fdir" }, f.dir || "·"),
      el("span", { class: "fname" }, f.name),
      el("span", { class: "fsize" },
        `${f.runs.length} run${f.runs.length === 1 ? "" : "s"} · ${(f.bytes / 1024).toFixed(0)}kB`)));
    f.runs.forEach((r) => {
      const item = el("div", { class: "run-item", "data-file": fi, "data-run": r.id, tabindex: "0", role: "button" });
      const dot = r.stats.success == null ? "neutral" : r.stats.success ? "pass" : "fail";
      item.append(el("span", { class: `outcome-dot ${dot}` }));
      item.append(el("span", { class: "run-title", title: r.id }, r.label));
      item.append(el("span", { class: "run-dur" }, fmtDur(r.stats.duration_s)));
      item.addEventListener("click", () => selectRun(fi, r.id));
      item.addEventListener("keydown", (e) => {
        if (e.key === "Enter" || e.key === " ") { e.preventDefault(); e.stopPropagation(); selectRun(fi, r.id); }
      });
      group.append(item);
    });
    list.append(group);
  });
}

/* ---------------- filters ---------------- */

function buildTypeFilters(run) {
  const wrap = $("#type-filters");
  wrap.innerHTML = "";
  const counts = new Map();
  const typeField = spec().trace.type_field;
  for (const ev of run.events) {
    const ty = String(getPath(ev, typeField) ?? "event");
    counts.set(ty, (counts.get(ty) || 0) + 1);
  }
  for (const [ty, n] of [...counts.entries()].sort((a, b) => b[1] - a[1])) {
    const on = !state.mutedTypes.has(ty);
    const btn = el("button", { class: "type-filter" + (on ? " on" : "") });
    btn.append(el("span", { class: "sw", style: `background:${toneColor(eventTone(ty))}` }));
    btn.append(document.createTextNode(eventLabel(ty)));
    btn.append(el("span", { class: "n" }, n));
    btn.addEventListener("click", () => {
      if (state.mutedTypes.has(ty)) state.mutedTypes.delete(ty);
      else state.mutedTypes.add(ty);
      buildTypeFilters(run);
      applyFilters();
    });
    wrap.append(btn);
  }
}

function eventText(ev) {
  try { return JSON.stringify(ev).toLowerCase(); } catch { return ""; }
}

function applyFilters() {
  const q = state.query.trim().toLowerCase();
  const run = state.run;
  let visible = 0;
  if (run) {
    run.events.forEach((ev, i) => {
      const ty = String(getPath(ev, spec().trace.type_field) ?? "event");
      const card = document.getElementById(cardId(ev, i));
      if (!card) return;
      const typeOk = !state.mutedTypes.has(ty);
      const textOk = !q || eventText(ev).includes(q);
      const show = typeOk && textOk;
      card.classList.toggle("hidden", !show);
      if (show) visible++;
    });
  }
  // table rows
  if (state.view === "table") {
    let rows = 0;
    document.querySelectorAll("#tablewrap tbody tr").forEach((tr) => {
      const ty = tr.getAttribute("data-type");
      const show = !state.mutedTypes.has(ty) && (!q || tr.textContent.toLowerCase().includes(q));
      tr.style.display = show ? "" : "none";
      if (show) rows++;
    });
    visible = rows;
  }
  $("#match-count").textContent = `${visible} shown`;
}

/* ---------------- table view ---------------- */

function renderTable(run) {
  const wrap = $("#tablewrap");
  wrap.innerHTML = "";
  const cols = (spec().table?.columns?.length ? spec().table.columns : null)
    || inferColumns(run);
  const table = el("table", { class: "grid" });
  const thead = el("thead", {}, el("tr", {}, ...cols.map((c) =>
    el("th", {}, spec().table?.labels?.[c] || c))));
  const tbody = el("tbody");
  run.events.forEach((ev, i) => {
    const ty = String(getPath(ev, spec().trace.type_field) ?? "event");
    const cond = conditionalStyle(ev);
    const tr = el("tr", { "data-type": ty });
    if (cond.tone === "red" || (ev.success === false && ty === "terminal")) tr.classList.add("row-fail");
    if (cond.tone === "emerald" || ev.success === true) tr.classList.add("row-pass");
    for (const col of cols) {
      const v = getPath(ev, col);
      const td = el("td");
      if (v != null) {
        if (typeof v === "object") {
          td.textContent = JSON.stringify(v);
          td.title = JSON.stringify(v, null, 2);
        } else if (col === spec().trace.timestamp_field || /^ts|_at$|timestamp/.test(col)) {
          const num = Number(v);
          td.textContent = isNaN(num) ? String(v) : fmtTs(num);
        } else {
          const vs = valueStyle(col, v);
          const label = vs?.label ?? String(v);
          if (vs) {
            const chip = el("span", { class: "vchip", style: vs.tone ? `color:${toneColor(vs.tone)}` : "" }, (vs.prefix || "") + label);
            td.append(chip);
          } else {
            td.textContent = typeof v === "string" && v.length > 200 ? v.slice(0, 200) + "…" : String(v);
            td.title = String(v);
          }
        }
      } else {
        td.textContent = "–";
        td.style.color = "var(--text-faint)";
      }
      tr.append(td);
    }
    tr.addEventListener("click", () => jumpToSeq(getPath(ev, spec().trace.sequence_field || "sequence")));
    tbody.append(tr);
  });
  table.append(thead, tbody);
  wrap.append(table);
  applyFilters();
}

function inferColumns(run) {
  const cols = new Set();
  for (const ev of run.events.slice(0, 200)) {
    for (const k of Object.keys(ev)) if (!isIgnored(k) && k !== "messages") cols.add(k);
  }
  return [...cols].slice(0, 12);
}

/* ---------------- view switching / run selection ---------------- */

function setView(v) {
  state.view = v;
  $("#tab-stream").classList.toggle("active", v === "stream");
  $("#tab-table").classList.toggle("active", v === "table");
  $("#stream").style.display = v === "stream" ? "" : "none";
  $("#tablewrap").style.display = v === "table" ? "" : "none";
  if (state.run) {
    if (v === "table") renderTable(state.run);
    else applyFilters();
  }
}

async function reloadTraces() {
  const response = await fetch("/api/reload", { method: "POST" });
  if (!response.ok) return;
  const previous = [state.fileIdx, state.runId];
  state.meta = await (await fetch("/api/meta")).json();
  renderSidebar();
  const file = state.meta.files[previous[0]];
  const run = file?.runs.find((item) => item.id === previous[1]);
  if (run) await selectRun(previous[0], run.id);
  else if (state.meta.files[0]?.runs[0]) await selectRun(0, state.meta.files[0].runs[0].id);
}

async function selectRun(fileIdx, runId) {
  state.fileIdx = fileIdx;
  state.runId = runId;
  state.mutedTypes = new Set();
  document.querySelectorAll(".run-item").forEach((n) => {
    n.classList.toggle("active",
      +n.getAttribute("data-file") === fileIdx && n.getAttribute("data-run") === runId);
  });
  const res = await fetch(`/api/trace/${fileIdx}`);
  const file = await res.json();
  const run = file.runs.find((r) => r.id === runId);
  if (!run) return;
  state.run = run;
  state.query = "";
  $("#search").value = "";
  renderRunHead(run);
  renderTimeline(run);
  buildTypeFilters(run);
  renderStream(run);
  setView(state.view);
  $("#content").scrollTop = 0;
}

/* ---------------- keyboard controls ---------------- */

function switchRun(delta) {
  if (!state.meta?.files?.length) return;
  const runs = [];
  state.meta.files.forEach((file, fileIdx) => file.runs.forEach((run) => runs.push([fileIdx, run.id])));
  const current = runs.findIndex(([fileIdx, id]) => fileIdx === state.fileIdx && id === state.runId);
  if (current < 0) return;
  const next = (current + delta + runs.length) % runs.length;
  selectRun(runs[next][0], runs[next][1]);
}

function focusRun(delta = 0) {
  const items = [...document.querySelectorAll(".run-item")];
  if (!items.length) return;
  const selected = document.querySelector(".run-item.active");
  const current = items.indexOf(selected || document.activeElement);
  const next = Math.max(0, Math.min(items.length - 1, (current < 0 ? 0 : current) + delta));
  state.focus = "runs";
  if (delta !== 0 && next !== current) {
    items[next].click();
    setTimeout(() => document.querySelector(".run-item.active")?.focus(), 0);
  } else {
    items[next].focus();
  }
}

function focusEvent(delta = 0) {
  const cards = [...document.querySelectorAll("#stream .ev:not(.hidden)")];
  if (!cards.length) return;
  const currentCard = document.activeElement?.closest(".ev");
  const current = cards.indexOf(currentCard);
  const next = Math.max(0, Math.min(cards.length - 1, (current < 0 ? 0 : current) + delta));
  state.focus = "events";
  cards[next].querySelector(".ev-head")?.focus();
}

function showKeyboardHelp(show = true) {
  const modal = $("#keyboard-help");
  modal.hidden = !show;
  if (show) $("#close-help").focus();
}

function handleKeyboard(e) {
  const target = e.target;
  const typing = target.matches("input, textarea, select, [contenteditable]");
  if (typing) {
    if (e.key === "Escape") { target.blur(); }
    return;
  }
  if (!$("#keyboard-help").hidden) {
    if (e.key === "Escape" || e.key === "?" || e.key === "q") showKeyboardHelp(false);
    return;
  }
  if (e.key === "?") { e.preventDefault(); showKeyboardHelp(); return; }
  if (e.ctrlKey && (e.key === "[" || e.key === "]")) {
    e.preventDefault(); switchRun(e.key === "[" ? -1 : 1); return;
  }
  if (e.key === "t") { e.preventDefault(); setView(state.view === "stream" ? "table" : "stream"); return; }
  if (e.key === "/") { e.preventDefault(); $("#search").focus(); $("#search").select(); return; }
  if (e.key === "b") { e.preventDefault(); $("#app").classList.toggle("sidebar-collapsed"); return; }
  if (e.key === "h") { e.preventDefault(); state.focus = "runs"; focusRun(); return; }
  if (e.key === "l") { e.preventDefault(); state.focus = "events"; focusEvent(); return; }
  if (e.key === "r" && state.run) { e.preventDefault(); reloadTraces(); return; }
  if ((e.key === "n" || e.key === "N") && state.query) {
    e.preventDefault(); state.focus = "events"; focusEvent(e.key === "n" ? 1 : -1); return;
  }
  if (e.key === "g") {
    if (state.pendingG) {
      state.pendingG = false;
      e.preventDefault();
      if (state.focus === "runs") focusRun(-999999);
      else focusEvent(-999999);
    } else {
      state.pendingG = true;
      setTimeout(() => { state.pendingG = false; }, 600);
    }
    return;
  }
  if (e.key === "G" || e.key === "End") { e.preventDefault(); state.focus === "runs" ? focusRun(999999) : focusEvent(999999); return; }
  if (e.key === "Home") { e.preventDefault(); state.focus === "runs" ? focusRun(-999999) : focusEvent(-999999); return; }
  if (e.key === "j" || e.key === "ArrowDown") { e.preventDefault(); state.focus === "runs" ? focusRun(1) : focusEvent(1); return; }
  if (e.key === "k" || e.key === "ArrowUp") { e.preventDefault(); state.focus === "runs" ? focusRun(-1) : focusEvent(-1); return; }
  if (e.key === "PageDown" || e.key === "PageUp") {
    e.preventDefault(); const n = e.key === "PageDown" ? 10 : -10;
    state.focus === "runs" ? focusRun(n) : focusEvent(n); return;
  }
  if (e.key === "Escape") { state.query = ""; $("#search").value = ""; applyFilters(); }
}

/* ---------------- boot ---------------- */

async function boot() {
  state.meta = await (await fetch("/api/meta")).json();
  renderSidebar();
  if (location.hash === "#table") state.view = "table";
  // deep link: #run=<id substring> selects that run across all files
  const wantRun = location.hash.startsWith("#run=") ? decodeURIComponent(location.hash.slice(5)) : null;
  let picked = null;
  if (wantRun) {
    outer: for (let fi = 0; fi < state.meta.files.length; fi++) {
      const runs = state.meta.files[fi].runs;
      const hit = runs.find((r) => r.id.includes(wantRun));
      if (hit) { picked = [fi, hit.id]; break outer; }
    }
  }
  if (picked) selectRun(picked[0], picked[1]);
  else {
    const f = state.meta.files[0];
    if (f && f.runs.length) selectRun(0, f.runs[0].id);
  }

  $("#search").addEventListener("input", (e) => { state.query = e.target.value; applyFilters(); });
  $("#close-help").addEventListener("click", () => showKeyboardHelp(false));
  $("#keyboard-help").addEventListener("click", (e) => { if (e.target.id === "keyboard-help") showKeyboardHelp(false); });
  document.addEventListener("keydown", handleKeyboard);
  $("#tab-stream").addEventListener("click", () => setView("stream"));
  $("#tab-table").addEventListener("click", () => setView("table"));
}

boot();
