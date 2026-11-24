# Podcasters and commentators

A calm, branded talk-show picture: the host clearly visible, the show's name and episode topic on screen, guests introduced properly, captions or quotes when they help the listener.

## Background

- `set_background {"full": "image"}` after `upload_background_image` with the show's artwork or studio picture: a branded, steady look.
- Or `{"full": "colour", "full_colour": "#14213d"}` for a clean set; or `blurred-room` if the real studio looks good.
- Commentary over a match or a clip: the page or `youtube` only with content the user may rebroadcast.

## Webcam

- `set_overlay {"style": "cutout_full", "x": 0.3}`: host left of centre, room on the right for quotes and charts.
- With a single camera for host and guest in one room, use `box` at `{"x": 0.5, "width": 0.6}`.

## Graphics

```
add_layer {"kind": "template", "template": "lower-third", "params": {"title": "Sam Rivera", "subtitle": "Host, The Margin Notes"}, "hide_after_s": 15}
add_layer {"kind": "template", "template": "section-title", "params": {"number": "01", "kicker": "Episode 48", "title": "Is remote work here to stay?"}, "visible": false}
add_layer {"kind": "template", "template": "live-badge", "params": {"label": "ON AIR"}, "visible": false}
add_layer {"kind": "template", "template": "quote-card", "params": {"quote": "Most teams did not choose remote work, they inherited it.", "author": "Dana Brooks", "role": "Guest, workplace researcher"}, "x": 0.45, "y": 0.62, "w": 0.5, "visible": false}
add_layer {"kind": "template", "template": "question-card", "params": {"question": "How do you keep junior staff learning when nobody is in the office?", "source": "via YouTube"}, "x": 0.45, "y": 0.6, "visible": false}
add_layer {"kind": "template", "template": "social-handles", "params": {"handles": [["YouTube", "@marginnotes"], ["Web", "marginnotes.fm"]], "layout": "row"}, "x": 0.45, "y": 0.86, "w": 0.5, "h": 0.08, "visible": false}
```

## Steps

1. `status`, `start` if stopped; background and webcam; `snapshot`.
2. Cold open (first minute): live-badge on, section-title full frame for 5-10 seconds with the episode topic, then hide it. Host lower third (hides after 15 s).
3. Guest intro: `update_layer {"id": "lower-third", "params": {"title": "Dana Brooks", "subtitle": "Workplace researcher"}}` then `show_layer`. Save as `save_scene {"name": "main", "include_background": true, "include_webcam": true}`.
4. Segments: change the section title between topics (`update_layer {"id": "section-title", "params": {"number": "02", "title": "What managers get wrong"}}`), show it briefly, hide it.
5. Highlight a strong line with quote-card for 10-20 seconds, then hide. Only quote what was actually said, with the speaker's agreement.
6. Listener questions (last 10 minutes): question-card, one at a time; leave `name` empty unless the asker agreed to be named.
7. Close: social handles, a thank-you card if wanted, then `clear_layers`.

## Rules

- Audio first: check `set_audio` (mic on, page sound off unless playing a clip).
- Keep graphics on the side away from the host's face; check with `snapshot` after each change.
- One moving element at a time; no tickers while someone is talking.
- 5% safe margin for all text.
- Do not show guests' names or titles until they confirm the spelling.
- Clear graphics at the end.
