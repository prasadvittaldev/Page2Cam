# Cooking, craft and DIY creators

A practical how-to picture: the work surface is the star, the recipe or materials are always findable, and the current step and timer are on screen so viewers can follow along.

## Background

- The webcam usually points at the bench or stove: keep the room real. `set_background {"full": "page"}` with the webcam as a large `box`, `in_box` room.
- Or a second view: a phone camera app or document camera page via `open_url`, with the person in a small box.
- `screen` only for a pattern or plan on a monitor that shows nothing else (it shares the whole monitor).

## Webcam

- Over-the-bench camera: `set_overlay {"style": "box", "x": 0.05, "y": 0.5, "width": 0.6}` with graphics on the right.
- Talking to camera: `{"style": "cutout_small", "x": 0.05}`.

## Graphics

```
add_layer {"kind": "template", "template": "recipe-card", "params": {"title": "Lemon garlic pasta", "meta": "Serves 4 · 25 min", "ingredients": ["400 g spaghetti", "3 cloves garlic", "1 lemon", "60 g butter", "50 g parmesan"], "step": "Boil the spaghetti in salted water for 9 minutes.", "step_label": "Step 1"}}
add_layer {"kind": "template", "template": "countdown", "params": {"label": "Pasta ready in", "duration": "9:00", "done_text": "Drain now"}, "visible": false}
add_layer {"kind": "template", "template": "steps", "params": {"steps": ["Measure", "Cut", "Glue", "Clamp", "Sand"], "current": 1}, "visible": false}
add_layer {"kind": "template", "template": "checklist", "params": {"title": "You will need", "items": [["Pine board 60 cm", "no"], ["Wood glue", "no"], ["2 clamps", "no"], ["120 grit sandpaper", "no"]]}, "visible": false}
add_layer {"kind": "template", "template": "lower-third", "params": {"title": "Marco Bellini", "subtitle": "Weeknight cooking"}, "hide_after_s": 15, "visible": false}
```

Use recipe-card for cooking, steps and checklist for crafts and DIY; not both.

## Steps

1. `status`, `start` if stopped; webcam and background; `snapshot` to check the bench is in view.
2. Intro (first minute): lower third with the creator's name; recipe-card or the materials checklist so viewers can gather things.
3. Main: advance live as the work goes on: `update_layer {"id": "recipe-card", "params": {"step": "Fry the sliced garlic in butter until golden.", "step_label": "Step 2"}}` or `update_layer {"id": "steps", "params": {"current": 3}}`. Tick materials as they are used.
4. Timers: show countdown when something cooks, sets or dries; hide it when done. One timer at a time.
5. Save the main look: `save_scene {"name": "bench", "include_background": true, "include_webcam": true}`; a talking-head look as `"talk"`, switch with `load_scene`.
6. Q&A at the end: talk-to-camera scene, question-card for viewer questions.
7. Close: thank-you card if wanted, then `clear_layers`.

## Rules

- Keep the hands and the work area clear: graphics go on the side the camera does not need; check with `snapshot`.
- One moving element at a time (no countdown while the steps bar animates).
- Safety steps (hot oil, power tools) stay readable: never cover them with a card.
- 5% safe margin for text.
- Clear graphics at the end.
