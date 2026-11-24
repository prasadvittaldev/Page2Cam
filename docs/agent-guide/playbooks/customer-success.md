# Customer onboarding, training and office hours

Friendly, practical sessions for customers: where they are in onboarding, the next task shown step by step in the product, shortcuts to remember, and their questions answered one at a time.

## Background

- The product on a demo or training account with `open_url`, or `screen` on a clean monitor. `screen` shares the WHOLE monitor: never the one with the CRM, support tickets or another customer's account; Do Not Disturb on.
- Welcome and office-hours chat: `{"full": "colour", "full_colour": "#0d1b2a"}` or `blurred-room`.

## Webcam

- Product: `set_overlay {"style": "cutout_small", "x": 0.95}`.
- Welcome and Q&A: `{"style": "cutout_full", "x": 0.3}`.

## Graphics

```
add_layer {"kind": "template", "template": "lower-third", "params": {"title": "Nina Berg", "subtitle": "Customer Success, Example Co"}, "hide_after_s": 15}
add_layer {"kind": "template", "template": "steps", "params": {"title": "Your onboarding", "steps": ["Connect data", "Invite team", "First dashboard", "Automations", "Go live"], "current": 1}, "visible": false}
add_layer {"kind": "template", "template": "callout", "params": {"label": "Click Invite", "direction": "right"}, "x": 0.5, "y": 0.2, "visible": false}
add_layer {"kind": "template", "template": "keyboard-shortcut", "params": {"keys": ["Ctrl", "K"], "caption": "Search everywhere"}, "visible": false}
add_layer {"kind": "template", "template": "question-card", "params": {"question": "Can we import from our old CSV exports?", "source": "via chat"}, "visible": false}
add_layer {"kind": "template", "template": "qr-card", "params": {"link": "https://help.example.com/start", "title": "Help centre", "caption": "help.example.com/start"}, "visible": false}
```

## Steps

1. `status`, `start` if stopped; colour background, cutout_full; `snapshot`. Save as `save_scene {"name": "welcome", "include_background": true, "include_webcam": true}`.
2. Welcome (first 3 minutes): lower third, then the onboarding steps at the customer's real stage: `update_layer {"id": "steps", "params": {"current": 2}}`.
3. Training: switch to the product, cutout_small, `save_scene {"name": "product", ...}`. For each task: a callout next to the control (`update_layer {"id": "callout", "params": {"label": "Choose a role"}}`), keyboard-shortcut when useful. Hide each before the next one.
4. Office hours: `load_scene {"id": "welcome", "mode": "replace"}`; question-card per question (no asker names unless they agreed), then answer in the product.
5. Close: steps updated to where the customer is now, qr-card for the help centre, `clear_layers`.

## Rules

- Never show another customer's name, logo, data or account; in group sessions never show one customer's data to others. Use a demo account.
- No health scores, renewal dates, pricing or internal notes on screen.
- One moving element at a time: callout pulse or the shortcut, not both.
- Callouts next to the control, never over the face; 5% safe margin; check every change with `snapshot`.
- Clear graphics at the end.
