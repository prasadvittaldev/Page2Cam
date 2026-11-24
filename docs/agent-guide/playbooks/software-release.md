# Software product demos and launches

A launch that looks like a product team made it: the version announced, what's new in one panel, the features shown live on a shared screen with callouts and shortcuts, the roadmap after, and a clear way to try it.

## Background

- Feature walk-through: `set_background {"full": "screen", "screen": "DISPLAY2"}` (`list_screens` first) with the product on a clean demo account. It shares the WHOLE monitor: close Slack, email, the admin console and real customer data, and turn on Do Not Disturb.
- Or the web app with `open_url` (a demo or staging account, never production with customer data).
- Announcement and roadmap: `{"full": "colour", "full_colour": "#0d1b2a"}` or a launch image.

## Webcam

- Demo: `set_overlay {"style": "cutout_small", "x": 0.95}`; move to `"x": 0.05` if the app's important controls are bottom right.
- Announcement: `{"style": "cutout_full", "x": 0.3}`, release panel on the right.

## Graphics

```
add_layer {"kind": "template", "template": "version-badge", "params": {"version": "v3.0", "tagline": "Real-time collaboration"}}
add_layer {"kind": "template", "template": "release-notes", "params": {"version": "v3.0", "date": "24 Nov 2026", "new": ["Live co-editing", "Comments on any block"], "improved": ["2x faster search", "Offline mode on mobile"], "fixed": ["Export keeps images"]}, "visible": false}
add_layer {"kind": "template", "template": "callout", "params": {"label": "New: Share live", "direction": "up"}, "x": 0.72, "y": 0.12, "w": 0.14, "h": 0.25, "visible": false}
add_layer {"kind": "template", "template": "keyboard-shortcut", "params": {"keys": ["Ctrl", "K"], "caption": "Jump to anything"}, "visible": false}
add_layer {"kind": "template", "template": "roadmap-timeline", "params": {"title": "What's next", "milestones": [["v3.0 co-editing", "Nov", "done"], ["Public API", "Jan", "now"], ["Desktop app", "Mar", "next"], ["Enterprise SSO", "Q2", "later"]]}, "visible": false}
add_layer {"kind": "template", "template": "qr-card", "params": {"link": "https://example.com/try", "title": "Try v3.0", "caption": "example.com/try"}, "visible": false}
```

## Steps

1. `status`, `start` if stopped; colour background, cutout_full; `snapshot`. `save_scene {"name": "announce", "include_background": true, "include_webcam": true}`.
2. Countdown before a public launch: starting-soon with `until` the launch time.
3. Announcement (2-3 minutes): version-badge, then release-notes for the headline list; hide the badge while the notes show.
4. Walk-through: switch to the demo (screen or `open_url`), cutout_small, `save_scene {"name": "demo", ...}`. Per feature: a callout placed next to the control (`update_layer {"id": "callout", "params": {"label": "Then pick who can edit"}}`), a keyboard-shortcut when one is used. Hide each before the next.
5. A late fix that made the release: `update_layer {"id": "release-notes", "params": {"fixed": ["Export keeps images", "Login loop on Safari"]}}`.
6. What's next: `load_scene {"id": "announce", "mode": "replace"}`, roadmap-timeline (only dates the user approved for public use).
7. Q&A: question-card per question. Close: qr-card to try it, thank-you card, `clear_layers`.

## Rules

- Never show real customer data, API keys, tokens, internal URLs or unreleased features the user did not clear; use a demo account.
- `screen` shares the whole monitor: check it with `snapshot` before switching and after every app change.
- One moving element at a time: one callout pulse, no typing terminal beside it.
- Callouts next to the control, never over the presenter's face; 5% safe margin.
- Roadmap dates are commitments: only what the user says is public.
- Clear graphics at the end.
