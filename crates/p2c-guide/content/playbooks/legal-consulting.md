---
title: Lawyers and consultants
group: business
aliases: lawyer, attorney, solicitor, barrister, law firm, legal, consultant, consulting, paralegal, notary, mediator, management consultant
prompt: Set up for a client meeting
args: name, title, company
---
# Lawyers and consultants

A discreet, serious picture for client meetings, advice sessions and workshops: the professional clearly visible, the firm's name once, and no client information on screen at any time.

## Background

- `set_background {"full": "blurred-room"}`: files, whiteboards and screens behind the user become unreadable. Preferred for every client meeting.
- Or a plain colour (`{"full": "colour", "full_colour": "#1c2533"}`) or the firm's neutral brand image.
- Never `screen` while a case file, contract, email or document system could be open on that monitor (it shares the whole monitor). For a document walk-through, `open_url` with only that document, after the user confirms it may be shown.

## Webcam

- `set_overlay {"style": "cutout_full", "x": 0.5}`: centred. Over a shared document: `{"style": "cutout_small", "x": 0.95}`.

## Graphics

```
add_layer {"kind": "template", "template": "lower-third", "params": {"title": "Helena Novak", "subtitle": "Partner, Example & Co LLP"}, "hide_after_s": 15}
add_layer {"kind": "template", "template": "confidential-badge", "params": {"text": "CONFIDENTIAL", "note": "Privileged, do not record"}, "visible": false}
add_layer {"kind": "template", "template": "agenda", "params": {"title": "Today", "items": ["Your goals", "Options", "Risks", "Next steps"], "current": 1}, "visible": false}
add_layer {"kind": "template", "template": "steps", "params": {"title": "How we work", "steps": ["Scoping", "Analysis", "Recommendation", "Implementation"], "current": 2}, "visible": false}
add_layer {"kind": "template", "template": "logo-bug", "params": {"image": "https://example.com/firm-logo.png", "opacity": 0.7}, "visible": false}
```

Agenda and steps hold generic headings only, never matter details.

## Steps

1. `status`, `start` if stopped; blurred room, cutout_full; `snapshot`.
2. If the user asks, or says the meeting is confidential or privileged: `show_layer {"id": "confidential-badge"}` and keep it on for the whole meeting.
3. Start: lower third (hides itself). Agenda for a structured session: show it at each change, `update_layer {"id": "agenda", "params": {"current": 2}}`, hide it in between.
4. Consulting workshops: steps for the engagement phases; `update_layer {"id": "steps", "params": {"current": 3}}` as it moves on.
5. Save the look: `save_scene {"name": "client-meeting", "include_background": true, "include_webcam": true}`; `load_scene` it before each meeting.
6. End: `clear_layers`.

## Rules

- Never show client names, case or matter numbers, parties, deadlines, amounts, advice or anything said in the meeting in a graphic, even if the user mentions them.
- Prefer the blurred-room background; never share a screen with client documents on it.
- Add the `confidential-badge` when asked or when the meeting is confidential.
- No tickers, countdowns or moving graphics.
- Check every change with `snapshot`; nothing may cover the face; 5% safe margin.
- Remind the user that recording needs every party's consent and may affect privilege; Page2Cam does not record.
- Clear graphics at the end.
