# Using templates

Templates are ready-made, tested graphics: lower thirds, timers, agendas, charts, QR cards, product cards, code cards and more. One short call adds one; changing a value updates it live.

1. Find one: `guide` with `catalogue` (built-ins with a line each), or `list_templates` (built-ins and the user's own; `category` filters: basics, time, cards, lists, data, cta, calm, software, mine).
2. Read its parameters: `guide` with the template id, or `get_template {"id": "…"}`.
3. Add it: `add_layer {"kind": "template", "template": "lower-third", "params": {"title": "Alex Kim", "subtitle": "Head of Product"}}`. It appears at once in its default place (give `x`, `y`, `w`, `h` to move it, `visible: false` to prepare it hidden). The answer has its layer `id`.
4. Change values live: `update_layer {"id": "lower-third", "params": {"subtitle": "Founder"}}`. Only the given params change; `null` puts one back to its default. The graphic is not reloaded: countdowns keep counting, charts animate to the new values.
5. Check with `snapshot`. Hide with `hide_layer`, bring back with `show_layer`.

Every template takes `theme` (`brand` navy and blue, `news` navy, white and red, `minimal` white text with light shading) and `accent` (#rrggbb). Use one theme for the whole show.

Parameter types: text, longtext, number, bool, colour (#rrggbb), choice, list (of texts), table (rows of cells: `[["Mon", "120"], ["Tue", "180"]]`), datetime (`18:30` or `2026-12-31T18:30`), duration (seconds or `m:ss`), url, asset (a picture: an https address, a `data:image/…;base64,` URI, or `{"path": "C:\\Pictures\\logo.png"}`, which is read and embedded, up to 1 MB), qr (a link or text shown as a QR code).

Full-frame templates (starting-soon, be-right-back, thank-you, speaker-intro, section-title) cover the whole picture, the person included: use them between parts of the show, not while the person talks.

A template whose user template was deleted shows "template missing" and `list_layers` marks it `template_missing: true`.
