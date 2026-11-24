/* Page2Cam template runtime (P2C). Runs first in every template layer's
   sandboxed frame. Params are data: set text with textContent (P2C.text,
   P2C.el), never as HTML. */
(function () {
  "use strict";
  var P2C = (window.P2C = {});
  var handlers = [];
  var timers = {};
  var tweens = {};
  var lastJson = "";

  /** The safe area: keep text this far (fraction of the output) from the edges. */
  P2C.safe = { x: 0.05, y: 0.05 };

  var THEMES = {
    brand: { bg: "#0d1b2a", bg2: "#0b63f0", fg: "#ffffff", fg2: "#ffffff", muted: "#a9bdd6",
      accent: "#0b63f0", panel: "rgba(13,27,42,0.92)", line: "rgba(255,255,255,0.18)", shadow: "none" },
    news: { bg: "#0b2a5b", bg2: "#ffffff", fg: "#ffffff", fg2: "#0b2a5b", muted: "#c2cee0",
      accent: "#d7263d", panel: "rgba(11,42,91,0.94)", line: "rgba(255,255,255,0.2)", shadow: "none" },
    minimal: { bg: "rgba(0,0,0,0.45)", bg2: "rgba(255,255,255,0.16)", fg: "#ffffff", fg2: "#ffffff",
      muted: "#e2e2e2", accent: "#ffffff", panel: "rgba(0,0,0,0.38)", line: "rgba(255,255,255,0.3)",
      shadow: "0 0.08em 0.3em rgba(0,0,0,0.85)" }
  };

  var base = document.createElement("style");
  base.textContent =
    ":root{--font:'Segoe UI',system-ui,-apple-system,Roboto,Arial,sans-serif;--mono:Consolas,'Cascadia Mono','Courier New',monospace}" +
    "*{box-sizing:border-box}" +
    "html,body{margin:0;padding:0;width:100%;height:100%;overflow:hidden;background:transparent}" +
    "body{font-family:var(--font);color:var(--fg);text-shadow:var(--shadow);line-height:1.2}" +
    ".p2c-panel{background:var(--panel);color:var(--fg)}" +
    ".p2c-accent{color:var(--accent)}" +
    ".p2c-muted{color:var(--muted)}" +
    ".p2c-ellipsis{white-space:nowrap;overflow:hidden;text-overflow:ellipsis}" +
    ".p2c-safe{position:absolute;left:5vw;right:5vw;top:5vh;bottom:5vh}" +
    ".tok-k{color:#c792ea}.tok-s{color:#c3e88d}.tok-c{color:#7f8c98;font-style:italic}" +
    ".tok-n{color:#f78c6c}.tok-f{color:#82aaff}.tok-p{color:#89ddff}";
  document.head.appendChild(base);

  function applyTheme(p) {
    var name = THEMES[p.theme] ? p.theme : "brand";
    var t = THEMES[name];
    var s = document.documentElement.style;
    Object.keys(t).forEach(function (k) { s.setProperty("--" + k, t[k]); });
    if (typeof p.accent === "string" && /^#[0-9a-fA-F]{6}$/.test(p.accent)) s.setProperty("--accent", p.accent);
    var cls = document.documentElement.classList;
    Object.keys(THEMES).forEach(function (k) { cls.toggle("theme-" + k, k === name); });
  }

  function run(p) {
    handlers.forEach(function (fn) { fn(p); });
  }

  function readInitial() {
    var el = document.getElementById("p2c-params");
    try { return el ? JSON.parse(el.textContent || "{}") : {}; } catch (e) { return {}; }
  }

  P2C.params = readInitial();
  lastJson = JSON.stringify(P2C.params);
  applyTheme(P2C.params);

  /** Call fn(params) now and after every live change. */
  P2C.onParams = function (fn) {
    handlers.push(fn);
    fn(P2C.params);
  };

  window.addEventListener("message", function (e) {
    if (e.source !== window.parent) return;
    var d = e.data;
    if (!d || d.p2c !== "params" || !d.params || typeof d.params !== "object") return;
    var j = JSON.stringify(d.params);
    if (j === lastJson) return;
    lastJson = j;
    P2C.params = d.params;
    applyTheme(d.params);
    run(d.params);
  });

  // ------------------------------------------------------------ DOM
  P2C.$ = function (sel, root) { return (root || document).querySelector(sel); };
  P2C.$$ = function (sel, root) { return Array.prototype.slice.call((root || document).querySelectorAll(sel)); };
  P2C.text = function (el, s) { if (el) el.textContent = s === undefined || s === null ? "" : String(s); return el; };
  P2C.clear = function (el) { while (el && el.firstChild) el.removeChild(el.firstChild); return el; };
  /** A new element with a class and text (text set as text). */
  P2C.el = function (tag, cls, text, parent) {
    var e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text !== undefined && text !== null) e.textContent = String(text);
    if (parent) parent.appendChild(e);
    return e;
  };
  /** Show or hide an element. */
  P2C.show = function (el, on) { if (el) el.style.display = on ? "" : "none"; return el; };
  /** A picture address: http(s) or data:image only (else ""). */
  P2C.asset = function (v) {
    return typeof v === "string" && /^(https?:\/\/|data:image\/)/i.test(v.trim()) ? v.trim() : "";
  };
  /** Set an <img>'s picture, hiding it when there is none. */
  P2C.img = function (el, v) {
    var u = P2C.asset(v);
    if (u) { if (el.getAttribute("src") !== u) el.setAttribute("src", u); el.style.display = ""; }
    else { el.removeAttribute("src"); el.style.display = "none"; }
    return !!u;
  };

  // ------------------------------------------------------------ numbers and time
  /** A number from a cell ("1,234", "12.5%", "$3k" -> 3000). NaN if none. */
  P2C.num = function (v) {
    if (typeof v === "number") return v;
    var s = String(v === undefined || v === null ? "" : v).replace(/[,\s$€£%]/g, "");
    var m = /^([-+]?\d*\.?\d+)([kmb])?$/i.exec(s);
    if (!m) return NaN;
    var n = parseFloat(m[1]);
    var mult = { k: 1e3, m: 1e6, b: 1e9 }[(m[2] || "").toLowerCase()] || 1;
    return n * mult;
  };
  /** 1234.5 -> "1,234.5" (decimals: fixed digits; default as needed up to 2). */
  P2C.fmtNum = function (n, decimals) {
    if (!isFinite(n)) return "";
    var o = decimals === undefined ? { maximumFractionDigits: 2 } : { minimumFractionDigits: decimals, maximumFractionDigits: decimals };
    return Number(n).toLocaleString("en-US", o);
  };
  /** Seconds -> "m:ss" or "h:mm:ss" (long: always hours). */
  P2C.fmtTime = function (sec, long) {
    sec = Math.max(0, Math.floor(sec || 0));
    var h = Math.floor(sec / 3600), m = Math.floor((sec % 3600) / 60), s = sec % 60;
    var ss = (s < 10 ? "0" : "") + s;
    if (h > 0 || long) return h + ":" + (m < 10 ? "0" : "") + m + ":" + ss;
    return m + ":" + ss;
  };
  /** A datetime param ("YYYY-MM-DDTHH:MM…" or "HH:MM", the next one) as ms since 1970; NaN if none. */
  P2C.when = function (v) {
    if (typeof v !== "string" || !v) return NaN;
    var m = /^(\d\d):(\d\d)$/.exec(v.trim());
    if (m) {
      var d = new Date();
      d.setHours(+m[1], +m[2], 0, 0);
      if (d.getTime() < Date.now() - 1000) d.setDate(d.getDate() + 1);
      return d.getTime();
    }
    return Date.parse(v);
  };
  /** Run fn now and every ms milliseconds, replacing an earlier timer with the same key. */
  P2C.every = function (key, ms, fn) {
    if (timers[key]) clearInterval(timers[key]);
    fn();
    timers[key] = setInterval(fn, ms);
  };
  P2C.stop = function (key) { if (timers[key]) { clearInterval(timers[key]); delete timers[key]; } };
  /** Animate a number from `from` to `to` over ms, calling fn(value); replaces a running tween with the same key. */
  P2C.tween = function (key, from, to, ms, fn) {
    if (tweens[key]) cancelAnimationFrame(tweens[key]);
    if (!isFinite(from) || !isFinite(to) || ms <= 0 || from === to) { fn(to); return; }
    var t0 = null;
    function step(t) {
      if (t0 === null) t0 = t;
      var k = Math.min(1, (t - t0) / ms);
      var e = 1 - Math.pow(1 - k, 3);
      fn(from + (to - from) * e);
      if (k < 1) tweens[key] = requestAnimationFrame(step); else delete tweens[key];
    }
    fn(from);
    tweens[key] = requestAnimationFrame(step);
  };

  // ------------------------------------------------------------ SVG and charts
  var NS = "http://www.w3.org/2000/svg";
  /** An SVG element with attributes. */
  P2C.svg = function (tag, attrs, parent) {
    var e = document.createElementNS(NS, tag);
    Object.keys(attrs || {}).forEach(function (k) { e.setAttribute(k, String(attrs[k])); });
    if (parent) parent.appendChild(e);
    return e;
  };
  /** Points "x,y x,y" for values in a w x h box (min..max, default from the data). */
  P2C.linePoints = function (values, w, h, min, max) {
    var v = values.filter(function (x) { return isFinite(x); });
    if (!v.length) return "";
    var lo = min === undefined ? Math.min.apply(null, v) : min;
    var hi = max === undefined ? Math.max.apply(null, v) : max;
    if (hi === lo) { hi = lo + 1; }
    return values.map(function (x, i) {
      var px = values.length === 1 ? w / 2 : (i * w) / (values.length - 1);
      var py = h - ((x - lo) / (hi - lo)) * h;
      return px.toFixed(1) + "," + py.toFixed(1);
    }).join(" ");
  };
  /** A ring showing frac (0..1) in an <svg> (cleared first). */
  P2C.donut = function (svgEl, frac, opts) {
    opts = opts || {};
    P2C.clear(svgEl);
    svgEl.setAttribute("viewBox", "0 0 100 100");
    var w = opts.width || 12, r = 50 - w / 2, c = 2 * Math.PI * r;
    frac = Math.max(0, Math.min(1, frac || 0));
    P2C.svg("circle", { cx: 50, cy: 50, r: r, fill: "none", stroke: opts.track || "rgba(255,255,255,0.18)", "stroke-width": w }, svgEl);
    P2C.svg("circle", { cx: 50, cy: 50, r: r, fill: "none", stroke: opts.colour || "var(--accent)", "stroke-width": w,
      "stroke-linecap": "round", "stroke-dasharray": (c * frac).toFixed(2) + " " + c.toFixed(2),
      transform: "rotate(-90 50 50)" }, svgEl);
  };
  /** Draw a qr param ({text, n, d}) into an <svg> (cleared first); false when empty. */
  P2C.qr = function (svgEl, v) {
    P2C.clear(svgEl);
    if (!v || !v.n || typeof v.d !== "string") return false;
    var q = 4, size = v.n + 2 * q;
    svgEl.setAttribute("viewBox", -q + " " + -q + " " + size + " " + size);
    svgEl.setAttribute("shape-rendering", "crispEdges");
    P2C.svg("rect", { x: -q, y: -q, width: size, height: size, fill: "#ffffff" }, svgEl);
    P2C.svg("path", { d: v.d, fill: "#000000" }, svgEl);
    return true;
  };

  // ------------------------------------------------------------ code
  var KW = {
    js: "async await break case catch class const continue default delete do else export extends false finally for from function if import in instanceof let new null of return static super switch this throw true try typeof undefined var void while yield",
    py: "and as assert async await break class continue def del elif else except False finally for from global if import in is lambda None nonlocal not or pass raise return self True try while with yield",
    rust: "as async await break const continue crate dyn else enum extern false fn for if impl in let loop match mod move mut pub ref return self Self static struct super trait true type unsafe use where while Some None Ok Err",
    go: "break case chan const continue default defer else fallthrough for func go goto if import interface map nil package range return select struct switch true false type var",
    java: "abstract boolean break byte case catch char class const continue default do double else enum extends final finally float for if implements import instanceof int interface long new null package private protected public return short static super switch this throw throws true false try void volatile while var record",
    cs: "abstract as async await base bool break byte case catch char class const continue decimal default delegate do double else enum event false finally float for foreach get if in int interface internal is long namespace new null object out override private protected public readonly ref return set static string struct switch this throw true try using var void while",
    sql: "select from where and or not insert into values update set delete create table index join left right inner outer on group by order having limit as distinct null is in like between case when then else end with union all count sum avg min max primary key",
    sh: "if then else elif fi for while do done case esac in function return export local echo cd sudo set unset",
    json: "true false null"
  };
  var ALIAS = { javascript: "js", ts: "js", typescript: "js", tsx: "js", jsx: "js", python: "py", rs: "rust", golang: "go",
    "c#": "cs", csharp: "cs", bash: "sh", shell: "sh", zsh: "sh", powershell: "sh", ps1: "sh", kotlin: "java", c: "cs", cpp: "cs", "c++": "cs" };
  /** Fill el with `src` highlighted for `lang` (spans with tok-* classes; text only). */
  P2C.code = function (el, src, lang) {
    P2C.clear(el);
    lang = String(lang || "").toLowerCase();
    lang = ALIAS[lang] || lang;
    var kw = {};
    (KW[lang] || "").split(" ").forEach(function (k) { if (k) kw[lang === "sql" ? k.toLowerCase() : k] = 1; });
    var hashComment = lang === "py" || lang === "sh";
    var re = /(\/\/[^\n]*|\/\*[\s\S]*?\*\/|--[^\n]*|#[^\n]*)|("(?:\\.|[^"\\\n])*"|'(?:\\.|[^'\\\n])*'|`(?:\\.|[^`\\])*`)|(\b\d+(?:\.\d+)?\b)|([A-Za-z_$][\w$]*)|([^\w\s])|(\s+)/g;
    var s = String(src || ""), m, last = 0;
    function put(cls, t) { if (cls) P2C.el("span", cls, t, el); else el.appendChild(document.createTextNode(t)); }
    while ((m = re.exec(s))) {
      if (m.index > last) put("", s.slice(last, m.index));
      last = re.lastIndex;
      if (m[1]) {
        var c = m[1];
        var isComment = (c[0] === "/" && lang !== "sh") || (c[0] === "-" && lang === "sql") || (c[0] === "#" && hashComment);
        if (isComment) { put("tok-c", c); }
        else { re.lastIndex = m.index + 1; last = m.index + 1; put("tok-p", c[0]); }
      } else if (m[2]) put("tok-s", m[2]);
      else if (m[3]) put("tok-n", m[3]);
      else if (m[4]) {
        var w = m[4], key = lang === "sql" ? w.toLowerCase() : w;
        if (kw[key]) put("tok-k", w);
        else if (s.charAt(re.lastIndex) === "(") put("tok-f", w);
        else put("", w);
      } else if (m[5]) put("tok-p", m[5]);
      else put("", m[6]);
    }
    if (last < s.length) put("", s.slice(last));
  };
})();
