# Writing your own templates

When no built-in fits, write a template once and reuse it with different values.

A template is HTML (with its own `<style>` and `<script>`, up to 256 KB) that runs in a sandboxed frame the size of the layer, on a transparent background, after a small runtime called `P2C`. Its parameters are data, given as JSON.

```
save_template {"id": "sponsor-strip", "name": "Sponsor strip", "description": "Thanks to one sponsor",
  "rect": [0.05, 0.05, 0.3, 0.1],
  "params": [{"name": "sponsor", "type": "text", "required": true, "help": "Sponsor name"}],
  "example": {"sponsor": "Example Co"},
  "html": "<style>div{font:700 40vh var(--font);color:var(--fg);background:var(--panel);height:100%;display:flex;align-items:center;padding:0 4vh;border-left:3vh solid var(--accent)}</style><div id=s></div><script>P2C.onParams(function(p){P2C.text(P2C.$('#s'), 'Thanks to ' + p.sponsor)})</script>"}
```

Rules that make templates work well:
- Read params only through `P2C.onParams(function (p) { … })`. It runs at load and again on every live change with all params, so update elements in place (don't append duplicates).
- Set text with `P2C.text(el, s)` or `P2C.el(tag, class, text)`, never as HTML.
- Size with `vh`/`vw` (they are fractions of the layer's box), so the graphic scales with the layer.
- Use the theme's CSS variables: `--bg`, `--bg2`, `--fg`, `--fg2`, `--muted`, `--accent`, `--panel`, `--font`, `--mono`.
- Helpers: `P2C.$`, `P2C.el`, `P2C.clear`, `P2C.show`, `P2C.img(el, asset)`, `P2C.num`, `P2C.fmtNum`, `P2C.fmtTime`, `P2C.when(datetime)`, `P2C.every(key, ms, fn)`, `P2C.stop(key)`, `P2C.tween(key, from, to, ms, fn)`, `P2C.svg`, `P2C.linePoints`, `P2C.donut`, `P2C.qr(svg, qrParam)`, `P2C.code(el, src, lang)`.
- Parameter types: text, longtext, number (min, max), bool, colour, choice (options), list, table (columns), datetime, duration, url, asset, qr.

Other ways to make one:
- From an html layer you already made: `save_template {"from_layer": "<layer id>", "name": "…"}` (no parameters).
- From a built-in, changed: `save_template {"copy_of": "lower-third", "id": "my-lower-third", "html": "…"}` (any field you give replaces the built-in's).
- Saving with an existing id of your own replaces it (layers using it reload). `delete_template` removes it.

The control page's Templates tab exports a template to a `.p2c-template.json` file and imports one; that file is the same JSON as `get_template` with its HTML. Imported templates run sandboxed like html layers: they cannot reach Page2Cam or the user's files.
