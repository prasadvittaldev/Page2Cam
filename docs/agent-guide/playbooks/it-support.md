# IT support and helpdesks

A clear, step-by-step picture for walking a user through a fix or a training: the screen being explained large, the technician small, the steps and shortcuts on screen so the user can repeat them later.

## Background

- `set_background {"full": "screen", "screen": "DISPLAY2"}` with a test machine or a clean VM on its own monitor (`list_screens` first). It shares the WHOLE monitor: never the one with the ticket system, admin consoles, passwords or other users' data.
- Or the documentation / knowledge-base page with `open_url`.
- Talking only: `blurred-room`.

## Webcam

- `set_overlay {"style": "cutout_small", "x": 0.95}`: technician bottom right, the screen readable.

## Graphics

```
add_layer {"kind": "template", "template": "steps", "params": {"title": "Reset your VPN", "steps": ["Open settings", "Remove profile", "Sign in again", "Test"], "current": 1}}
add_layer {"kind": "template", "template": "keyboard-shortcut", "params": {"keys": ["Win", "R"], "caption": "Open the Run box"}, "visible": false}
add_layer {"kind": "template", "template": "callout", "params": {"label": "Click Remove", "direction": "left"}, "x": 0.4, "y": 0.35, "visible": false}
add_layer {"kind": "template", "template": "terminal-card", "params": {"prompt": "PS C:\\>", "command": "ipconfig /flushdns", "output": "Successfully flushed the DNS Resolver Cache.", "title": "PowerShell"}, "visible": false}
add_layer {"kind": "template", "template": "status-card", "params": {"service": "Email", "status": "degraded", "incident": "Slow delivery for some mailboxes", "updates": [["09:40", "Provider investigating."]]}, "visible": false}
add_layer {"kind": "template", "template": "lower-third", "params": {"title": "Chris Moreau", "subtitle": "IT Service Desk"}, "hide_after_s": 15, "visible": false}
```

## Steps

1. `status`, `start` if stopped; `list_screens`, pick the clean monitor; webcam; `snapshot` to check nothing private is visible.
2. Intro (first minute): lower third, then the steps for the fix.
3. Walk-through: advance live, `update_layer {"id": "steps", "params": {"current": 2}}`. Point at a button with callout placed right next to it (`x`/`y` of the layer; check the spot with `snapshot`); show a shortcut with keyboard-shortcut; show a command and its expected output with terminal-card.
4. Outages: status-card for a team or company briefing; update as news comes in: `update_layer {"id": "status-card", "params": {"status": "operational", "updates": [["10:15", "Resolved, mail flowing normally."], ["09:40", "Provider investigating."]]}}`.
5. Save looks: `save_scene {"name": "walkthrough", "include_background": true, "include_webcam": true}` and `"briefing"`.
6. Close: the steps all ticked (`current` 5), then `clear_layers`.

## Rules

- Never show passwords, recovery codes, licence keys, IP plans, user names of other staff, tickets or admin consoles; type secrets only with screen sharing off.
- `screen` shares everything on that monitor, notifications too: turn on Do Not Disturb first.
- One element at a time: callout pulse or a typing terminal, not both.
- Keep the area being explained and the face clear; 5% safe margin; check with `snapshot`.
- Clear graphics at the end.
