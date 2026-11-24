//! The scene page the overlay browser shows: a transparent host page whose
//! own script (`scene.js`) draws titles, tickers, pictures, videos and
//! YouTube boxes from the polled state, and puts each HTML layer in a
//! sandboxed frame (`layer/<id>`), so agent code never runs in the host page.

/// `scene.html`: transparent, no content of its own.
pub const SCENE_HTML: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"><title>Page2Cam overlay</title>
<style>
html,body{margin:0;padding:0;width:100%;height:100%;overflow:hidden;background:transparent}
.layer{position:absolute;box-sizing:border-box;overflow:hidden;display:none;will-change:transform,opacity}
.fill{position:absolute;inset:0;width:100%;height:100%;border:0;background:transparent}
img.fill,video.fill{object-fit:contain}
.title{position:absolute;inset:0;display:flex;flex-direction:column;justify-content:center;font-family:"Segoe UI",Arial,sans-serif;line-height:1.15}
.title .main,.title .sub{white-space:nowrap;overflow:hidden;text-overflow:ellipsis;padding:0 .45em}
.title .main{font-weight:700}
.news .main{background:#0b2a5b;color:#fff;border-left:.3em solid #d7263d}
.news .sub{background:#fff;color:#0b2a5b;border-left:.3em solid #d7263d}
.minimal .main,.minimal .sub{color:#fff;text-shadow:0 .06em .25em rgba(0,0,0,.85)}
.brand .main{background:#0d1b2a;color:#fff;border-left:.3em solid #0b63f0}
.brand .sub{background:#0b63f0;color:#fff;border-left:.3em solid #0d1b2a}
.ticker{position:absolute;inset:0;overflow:hidden;display:flex;align-items:center;font-family:"Segoe UI",Arial,sans-serif;font-weight:600;white-space:nowrap}
.ticker .track{display:inline-block;will-change:transform}
.ticker.news{background:#0b2a5b;color:#fff;border-top:.12em solid #d7263d}
.ticker.minimal{background:rgba(0,0,0,.55);color:#fff}
.ticker.brand{background:#0d1b2a;color:#fff;border-top:.12em solid #0b63f0}
.ticker .sep{opacity:.6;padding:0 .9em}
</style></head>
<body><script src="scene.js"></script></body></html>
"#;

/// `scene.js`: polls `state` and applies changes; untouched layers keep
/// running (a ticker keeps scrolling, a video keeps playing).
pub const SCENE_JS: &str = r#""use strict";
(function () {
  var rev = -1;
  var els = {};      // id -> {el, v, layer, hideTimer, raf}
  var ANIM_MS = 500;

  function offTransform(a) {
    switch (a) {
      case "slide-left": return "translateX(-110%)";
      case "slide-right": return "translateX(110%)";
      case "slide-up": return "translateY(110%)";
      case "slide-down": return "translateY(-110%)";
      default: return "none";
    }
  }

  function px(frac, total) { return (frac * total) + "px"; }

  function place(e, l) {
    var W = window.innerWidth, H = window.innerHeight;
    var s = e.el.style;
    s.left = px(l.x, W); s.top = px(l.y, H);
    s.width = px(l.w, W); s.height = px(l.h, H);
    s.zIndex = String(1000 + l.z);
    var boxH = l.h * H;
    if (l.kind === "title") {
      var main = e.el.querySelector(".main"), sub = e.el.querySelector(".sub");
      if (main) main.style.fontSize = (sub ? boxH * 0.40 : boxH * 0.56) + "px";
      if (sub) sub.style.fontSize = (boxH * 0.26) + "px";
    } else if (l.kind === "ticker") {
      e.el.style.fontSize = (boxH * 0.5) + "px";
    }
  }

  function el(tag, cls) {
    var d = document.createElement(tag);
    if (cls) d.className = cls;
    return d;
  }

  function buildTicker(e, l, box) {
    var t = el("div", "ticker " + l.style);
    if (l.colour) t.style.color = l.colour;
    if (l.background) t.style.background = l.background;
    var track = el("div", "track");
    function run() {
      var span = el("span");
      l.items.forEach(function (it) {
        var a = el("span"); a.textContent = it; span.appendChild(a);
        var s = el("span", "sep"); s.textContent = "•"; span.appendChild(s);
      });
      return span;
    }
    // Two copies: when the first has scrolled out, start again (seamless).
    track.appendChild(run()); track.appendChild(run());
    t.appendChild(track); box.appendChild(t);
    var x = 0, last = null;
    function step(ts) {
      if (last !== null) {
        var half = track.scrollWidth / 2;
        x -= (l.speed * window.innerWidth / 1920) * (ts - last) / 1000;
        if (half > 0 && -x >= half) x += half;
        track.style.transform = "translateX(" + x + "px)";
      }
      last = ts;
      e.raf = requestAnimationFrame(step);
    }
    e.raf = requestAnimationFrame(step);
  }

  function build(e, l) {
    if (e.raf) { cancelAnimationFrame(e.raf); e.raf = 0; }
    var box = e.el;
    while (box.firstChild) box.removeChild(box.firstChild);
    if (l.kind === "title") {
      var t = el("div", "title " + l.style);
      var m = el("div", "main"); m.textContent = l.text;
      if (l.colour) m.style.color = l.colour;
      if (l.background) m.style.background = l.background;
      t.appendChild(m);
      if (l.subtitle) { var s = el("div", "sub"); s.textContent = l.subtitle; t.appendChild(s); }
      box.appendChild(t);
    } else if (l.kind === "ticker") {
      buildTicker(e, l, box);
    } else if (l.kind === "html" || l.kind === "template") {
      var f = el("iframe", "fill");
      f.setAttribute("sandbox", "allow-scripts");
      f.setAttribute("scrolling", "no");
      f.src = "layer/" + encodeURIComponent(l.id) + "?v=" + l.v;
      // Params that changed while the frame loaded reach it now.
      if (l.kind === "template") f.addEventListener("load", function () { post(e); });
      box.appendChild(f);
    } else if (l.kind === "image") {
      var i = el("img", "fill"); i.alt = ""; i.src = "asset/" + encodeURIComponent(l.asset);
      box.appendChild(i);
    } else if (l.kind === "video") {
      var v = el("video", "fill");
      v.muted = !!l.muted; v.loop = !!l.loop; v.autoplay = true; v.playsInline = true;
      v.src = l.asset ? "asset/" + encodeURIComponent(l.asset) : l.url;
      box.appendChild(v);
      var p = v.play(); if (p && p.catch) p.catch(function () {});
    } else if (l.kind === "youtube") {
      var y = el("iframe", "fill");
      var id = encodeURIComponent(l.video);
      y.src = "https://www.youtube-nocookie.com/embed/" + id + "?autoplay=1&controls=0&modestbranding=1&playsinline=1" +
        "&mute=" + (l.muted ? 1 : 0) + (l.loop ? "&loop=1&playlist=" + id : "") + (l.start_s ? "&start=" + l.start_s : "");
      y.setAttribute("sandbox", "allow-scripts allow-same-origin allow-presentation");
      y.setAttribute("allow", "autoplay; encrypted-media");
      y.setAttribute("referrerpolicy", "strict-origin-when-cross-origin");
      box.appendChild(y);
    }
  }

  // A template frame gets new params as a message (no reload: timers and
  // animations keep running). Its origin is opaque, hence "*".
  function post(e) {
    var f = e.el.querySelector("iframe");
    if (f && f.contentWindow && e.layer && e.layer.params) {
      f.contentWindow.postMessage({ p2c: "params", params: e.layer.params }, "*");
    }
  }

  function show(e, l) {
    var s = e.el.style;
    if (e.hideTimer) { clearTimeout(e.hideTimer); e.hideTimer = 0; }
    var wasShown = s.display === "block" && e.shown;
    e.shown = true;
    if (wasShown) {
      s.opacity = String(l.opacity);
      return;
    }
    s.transition = "none";
    s.display = "block";
    if (l.enter === "none") { s.opacity = String(l.opacity); s.transform = "none"; return; }
    s.opacity = "0";
    s.transform = offTransform(l.enter);
    void e.el.offsetWidth; // start the transition from here
    s.transition = "opacity " + ANIM_MS + "ms ease-out, transform " + ANIM_MS + "ms ease-out";
    s.opacity = String(l.opacity);
    s.transform = "none";
  }

  function hide(e, l, remove) {
    var s = e.el.style;
    e.shown = false;
    if (e.hideTimer) clearTimeout(e.hideTimer);
    var done = function () {
      e.hideTimer = 0;
      if (remove) { if (e.raf) cancelAnimationFrame(e.raf); e.el.remove(); }
      else { s.display = "none"; }
    };
    if (s.display !== "block" || l.exit === "none") { done(); return; }
    s.transition = "opacity " + ANIM_MS + "ms ease-in, transform " + ANIM_MS + "ms ease-in";
    s.opacity = "0";
    s.transform = offTransform(l.exit);
    e.hideTimer = setTimeout(done, ANIM_MS + 50);
  }

  function apply(state) {
    var seen = {};
    state.layers.forEach(function (l) {
      seen[l.id] = true;
      var e = els[l.id];
      if (!e) {
        e = els[l.id] = { el: el("div", "layer"), v: null, layer: l, hideTimer: 0, raf: 0, shown: false };
        document.body.appendChild(e.el);
      }
      var rebuild = e.v !== l.v, repost = !rebuild && l.kind === "template" && e.pv !== l.pv;
      e.layer = l;
      if (rebuild) { build(e, l); e.v = l.v; }
      if (rebuild || repost) e.pv = l.pv;
      if (repost) post(e);
      place(e, l);
      if (l.visible) show(e, l); else hide(e, l, false);
    });
    Object.keys(els).forEach(function (id) {
      if (!seen[id]) { hide(els[id], els[id].layer, true); delete els[id]; }
    });
  }

  function poll() {
    fetch("state?since=" + rev, { cache: "no-store" })
      .then(function (r) { return r.status === 200 ? r.json() : null; })
      .then(function (s) { if (s) { rev = s.rev; apply(s); } })
      .catch(function () {})
      .then(function () { setTimeout(poll, 250); });
  }
  window.addEventListener("resize", function () {
    Object.keys(els).forEach(function (id) { place(els[id], els[id].layer); });
  });
  poll();
})();
"#;

/// The document an `html` layer runs in (inside a sandboxed frame): the
/// fragment on a transparent, unscrolled page.
pub fn layer_page(fragment: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
         <style>html,body{{margin:0;padding:0;width:100%;height:100%;overflow:hidden;background:transparent}}</style>\
         </head><body>{fragment}</body></html>"
    )
}

/// The document a template layer runs in (inside the same sandboxed frame
/// as `html` layers): the `P2C` runtime, the parameters as inert JSON data
/// (`<` escaped, so no value can end the script element), then the
/// template's HTML.
pub fn template_page(
    t: &crate::template::Template,
    params: &serde_json::Map<String, serde_json::Value>,
) -> String {
    let json = serde_json::Value::Object(params.clone())
        .to_string()
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026");
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
         <style>html,body{{margin:0;padding:0;width:100%;height:100%;overflow:hidden;background:transparent}}</style>\
         <script type=\"application/json\" id=\"p2c-params\">{json}</script>\
         <script>{runtime}</script></head><body>{html}</body></html>",
        runtime = crate::template::RUNTIME_JS,
        html = t.html,
    )
}

/// What a template layer shows when its template was deleted.
pub fn missing_page(template: &str) -> String {
    let safe: String = template
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><style>\
         html,body{{margin:0;width:100%;height:100%;overflow:hidden;background:transparent}}\
         div{{position:absolute;inset:0;display:flex;align-items:center;justify-content:center;\
         border:2px dashed rgba(255,255,255,.6);background:rgba(0,0,0,.45);color:#fff;\
         font:600 14px 'Segoe UI',Arial,sans-serif;text-align:center}}</style></head>\
         <body><div>Template \u{201c}{safe}\u{201d} is missing</div></body></html>"
    )
}

/// The host page may run only its own script; media from our origin or
/// the web; frames: our layer pages and the YouTube embed.
pub const SCENE_CSP: &str = "default-src 'none'; script-src 'self'; style-src 'unsafe-inline'; \
     img-src 'self' data:; media-src 'self' https: http:; connect-src 'self'; \
     frame-src 'self' https://www.youtube-nocookie.com";

/// An HTML layer's page: sandboxed (opaque origin, scripts only) even when
/// opened directly; it may load from the web (live data), never reach our API
/// (no token, no same-origin reads).
pub const LAYER_CSP: &str =
    "sandbox allow-scripts; default-src * data: blob: 'unsafe-inline' 'unsafe-eval'";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scene_runs_only_its_own_script() {
        assert!(SCENE_HTML.contains(r#"<script src="scene.js"></script>"#));
        assert_eq!(SCENE_HTML.matches("<script").count(), 1);
        assert!(SCENE_CSP.contains("script-src 'self'"));
        assert!(!SCENE_CSP.contains("unsafe-eval"));
    }

    #[test]
    fn html_layers_are_sandboxed_frames() {
        assert!(SCENE_JS.contains(r#"f.setAttribute("sandbox", "allow-scripts");"#));
        assert!(!SCENE_JS.contains("allow-same-origin\");\n      f.src"));
        assert!(LAYER_CSP.starts_with("sandbox allow-scripts;"));
        let p = layer_page("<b>x</b>");
        assert!(p.contains("<body><b>x</b></body>"));
        assert!(p.contains("background:transparent"));
    }

    #[test]
    fn template_frames_get_params_by_message() {
        assert!(SCENE_JS.contains(r#"l.kind === "html" || l.kind === "template""#));
        assert!(SCENE_JS.contains(r#"postMessage({ p2c: "params", params: e.layer.params }, "*")"#));
        assert!(SCENE_JS.contains(r#"f.addEventListener("load""#));
        let t = crate::builtin("lower-third").unwrap();
        let p = template_page(t, &t.effective(&serde_json::Map::new()));
        assert!(p.contains(r#"<script type="application/json" id="p2c-params">"#));
        assert!(p.find("p2c-params").unwrap() < p.find("window.P2C").unwrap());
        assert!(missing_page("a\"<b>").contains("ab"));
    }

    #[test]
    fn text_goes_in_as_text() {
        // Titles and ticker items are set with textContent, never innerHTML.
        assert!(!SCENE_JS.contains("innerHTML"));
    }
}
