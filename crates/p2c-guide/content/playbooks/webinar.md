---
title: Webinar hosts and marketers
group: business
aliases: webinars, webinar host, marketer, marketing, online seminar, lead generation, masterclass, web seminar, online workshop, lunch and learn
prompt: Set up for a webinar
args: name, topic, company
---
# Webinar hosts and marketers

A polished, structured session: a countdown before the start, the host and speakers introduced, an agenda that moves, polls and audience questions on screen, and a clear call to action at the end.

## Background

- `open_url` with the slides (Google Slides, PowerPoint for the web, any page): presenter in front of the content.
- `{"full": "image"}` with a branded background (`upload_background_image`) for the opening and Q&A.
- `screen` only for a monitor that shows nothing private (it shares the whole monitor; turn notifications off).

## Webcam

- Slides: `set_overlay {"style": "cutout_small", "x": 0.95}`.
- Opening, speaker talks without slides, Q&A: `{"style": "cutout_full", "x": 0.3}` with graphics on the right.

## Graphics

```
add_layer {"kind": "template", "template": "starting-soon", "params": {"title": "Starting soon", "subtitle": "Webinar: Cutting churn in 30 days", "until": "16:00"}}
add_layer {"kind": "template", "template": "speaker-intro", "params": {"name": "Priya Shah", "role": "Head of Customer Marketing", "company": "Example Co", "bio": "Priya has run lifecycle programmes for B2B SaaS teams for ten years."}, "visible": false}
add_layer {"kind": "template", "template": "agenda", "params": {"title": "Today", "items": ["Why customers leave", "Three quick wins", "Case study", "Q&A"], "current": 1}, "visible": false}
add_layer {"kind": "template", "template": "poll-results", "params": {"question": "What is your churn rate today?", "options": [["Under 3%", "0"], ["3-7%", "0"], ["Over 7%", "0"], ["Not sure", "0"]]}, "visible": false}
add_layer {"kind": "template", "template": "question-card", "params": {"question": "Does this work for low-touch self-serve products too?", "source": "via chat"}, "visible": false}
add_layer {"kind": "template", "template": "qr-card", "params": {"link": "https://example.com/churn-guide", "title": "Get the guide", "caption": "example.com/churn-guide"}, "visible": false}
```

## Steps

1. `status`, `start` if stopped; branded image background, cutout_full; `snapshot`.
2. 10 minutes before: starting-soon with `until` the start time. `save_scene {"name": "intro", "include_background": true, "include_webcam": true}`.
3. Start: hide starting-soon; speaker-intro full frame for 10-15 seconds, then hide; a lower third if wanted.
4. Content: `open_url` with the slides, cutout_small. Show the agenda at each section change for a few seconds and move it: `update_layer {"id": "agenda", "params": {"current": 2}}`. Save as `"main"`.
5. Polls: show poll-results and feed the votes from the user's poll tool as they come in: `update_layer {"id": "poll-results", "params": {"options": [["Under 3%", "42"], ["3-7%", "77"], ["Over 7%", "31"], ["Not sure", "18"]]}}`.
6. Q&A (last 10-15 minutes): `load_scene {"id": "intro", "mode": "replace"}` or cutout_full; question-card one question at a time; name only if the attendee agreed.
7. Close: qr-card or thank-you card with the follow-up link for 1-2 minutes, then `clear_layers`.

## Rules

- Never show attendee emails, company lists or registration data.
- Only real poll numbers; never invent results.
- One moving element at a time: poll bars animate, so hide the agenda meanwhile.
- Graphics off the face and inside the 5% safe margin; check every change with `snapshot`.
- Clear graphics at the end.
