---
title: Developer streams and talks
group: software
aliases: live coding, developer, dev stream, devrel, dev-rel, developer advocate, programmer, coder, conference talk, tech talk, coding stream, open source, pair programming
prompt: Set up my live coding stream
args: name, topic, product
---
# Developer streams and talks

Live coding, tech talks and dev-rel streams: the editor readable, the developer small beside it, code and commands repeated in clean cards when the editor is too small, and a few community touches.

## Background

- `set_background {"full": "screen", "screen": "DISPLAY2"}` (`list_screens` first): editor and terminal on their own monitor. It shares the WHOLE monitor: close email, chat and password managers, hide `.env` files, tokens and SSH keys, turn on Do Not Disturb. Use a large editor font.
- Talks: slides with `open_url`. Intros and chat: `{"full": "colour", "full_colour": "#0d1117"}`.

## Webcam

- Coding: `set_overlay {"style": "cutout_small", "x": 0.95}`; move it if it hides the editor's bottom right (terminal, status bar).
- Talking and chat: `{"style": "cutout_full", "x": 0.3}`.

## Graphics

```
add_layer {"kind": "template", "template": "lower-third", "params": {"title": "Ravi Iyer", "subtitle": "Developer Advocate, Example Co"}, "hide_after_s": 15}
add_layer {"kind": "template", "template": "code-card", "params": {"code": "const res = await fetch('/api/items');\nconst items = await res.json();\nconsole.log(items.length);", "language": "js", "filename": "app.js"}, "visible": false}
add_layer {"kind": "template", "template": "terminal-card", "params": {"prompt": "~/demo $", "command": "npm test", "output": "PASS  src/items.test.js\nTests: 12 passed, 12 total", "title": "bash"}, "visible": false}
add_layer {"kind": "template", "template": "keyboard-shortcut", "params": {"keys": ["Ctrl", "Shift", "P"], "caption": "Command palette"}, "visible": false}
add_layer {"kind": "template", "template": "social-handles", "params": {"handles": [["GitHub", "example/demo-app"], ["YouTube", "@exampledev"], ["Discord", "discord.gg/example"]]}, "visible": false}
add_layer {"kind": "template", "template": "checklist", "params": {"title": "Today we build", "items": [["Fetch the items", "no"], ["Add caching", "no"], ["Write tests", "no"], ["Deploy", "no"]]}, "visible": false}
```

## Steps

1. `status`, `start` if stopped; colour background, cutout_full; `snapshot`. Save as `save_scene {"name": "chat", "include_background": true, "include_webcam": true}`.
2. Intro (2-5 minutes): lower third, then today's checklist (the plan).
3. Coding: switch to the screen, cutout_small; `save_scene {"name": "code", ...}`. Check the screen with `snapshot` for secrets before going on.
4. Key snippet or command too small to read: show code-card or terminal-card for 20-30 seconds, then hide: `update_layer {"id": "terminal-card", "params": {"command": "npm run deploy", "output": "Deployed to https://demo.example.com"}}`. Keyboard-shortcut when using one the audience may not know.
5. Progress: tick the plan, `update_layer {"id": "checklist", "params": {"items": [["Fetch the items", "yes"], ["Add caching", "yes"], ["Write tests", "no"], ["Deploy", "no"]]}}`.
6. Chat questions: `load_scene {"id": "chat", "mode": "replace"}`, question-card.
7. Talks: section-title between parts, a qr-card for the slides at the end. Close: social handles, `clear_layers`.

## Rules

- `screen` shares the whole monitor: secrets in terminals, `.env` files, tokens, cloud consoles and notifications are all visible. Use throwaway keys and a demo account; if a secret shows, tell the user to rotate it.
- Code cards with typing on: one at a time; no other moving graphics beside them.
- Graphics never over the editor's active line or the developer's face; 5% safe margin; check with `snapshot`.
- Clear graphics at the end.
