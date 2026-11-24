# Therapists and counsellors

A quiet, safe, private picture: the therapist warm and clearly visible, the room private, and almost nothing else. Graphics are the exception, not the rule.

## Background

- `set_background {"full": "blurred-room"}`: the room stays real and warm, but books, notes, screens and anything personal become unreadable. Preferred for every session.
- Or a soft plain colour (`{"full": "colour", "full_colour": "#e9ece6"}`) with `refined_edges` true.
- Never `screen` or a shared page during a session.

## Webcam

- `set_overlay {"style": "cutout_full", "x": 0.5}`: centred, eye level, the face clearly lit. Or a plain `box` with `in_box` blur if the cut-out flickers at the edges.

## Graphics

```
add_layer {"kind": "template", "template": "name-badge", "params": {"name": "Ana Ruiz", "line": "Counsellor"}, "visible": false}
add_layer {"kind": "template", "template": "lower-third", "params": {"title": "Ana Ruiz", "subtitle": "Registered counsellor", "theme": "minimal"}, "hide_after_s": 10, "visible": false}
add_layer {"kind": "template", "template": "breathing", "params": {"title": "Breathe with me", "inhale": 4, "hold": 4, "exhale": 6, "cycles": 5, "theme": "minimal"}, "visible": false}
```

Only the therapist's own name and role. Nothing else.

## Steps

1. Before the client joins: `status`, `start` if stopped; blurred room, cutout_full; `snapshot`. Save once as `save_scene {"name": "session", "include_background": true, "include_webcam": true}` and `load_scene {"id": "session", "mode": "replace"}` before each session.
2. First session with a new client: the lower third for a few seconds so they know who they are talking to (it hides itself). Otherwise no graphics at all.
3. During the session: nothing on screen. Do not add, change or move anything while the client is talking.
4. Breathing exercise: only if the therapist offers it and the client wants it. `show_layer {"id": "breathing"}`; slow it down if asked (`update_layer {"id": "breathing", "params": {"exhale": 8}}`); `hide_layer` as soon as it is done.
5. End: `clear_layers`.

## Rules

- Never show a client's name, initials, age, diagnosis, notes, homework, goals, or anything said in the session, in any graphic, even if the user mentions it.
- Prefer the blurred-room background; never share a screen or page during a session.
- No flashing, pulsing, scrolling or busy motion; no tickers, countdowns, timers, live badges or polls.
- The breathing guide only when the client wants it; never as decoration.
- Check every change with `snapshot`; nothing may cover the therapist's face.
- Remind the user that their professional rules on consent and recording still apply; Page2Cam does not record.
