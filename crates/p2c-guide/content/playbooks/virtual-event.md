---
title: Conferences and virtual events
group: community
aliases: conference, summit, meetup, event host, organiser, organizer, panel, moderator, emcee, mc, hackathon, awards night, virtual conference
prompt: Set up our virtual event
args: event, name, company
---
# Conferences and virtual events

A broadcast-style event: a branded holding screen, speakers introduced with cards, timers that keep the schedule, sponsors thanked, questions from the audience, and clean transitions between sessions.

## Background

- `set_background {"full": "image"}` with the event's branded backdrop (`upload_background_image`) for the host and panels.
- Speaker slides with `open_url`; a pre-recorded talk with `{"full": "youtube", "youtube": "<link>"}` (its sound goes to the stream only).
- `screen` only for a clean presentation monitor (it shares the whole monitor).

## Webcam

- Host and moderator: `set_overlay {"style": "cutout_full", "x": 0.3}`.
- During slides: `{"style": "cutout_small", "x": 0.95}`.

## Graphics

```
add_layer {"kind": "template", "template": "starting-soon", "params": {"title": "DevSummit 2026", "subtitle": "Day 2 starts soon", "until": "09:00"}}
add_layer {"kind": "template", "template": "speaker-intro", "params": {"name": "Dr. Maya Chen", "role": "Principal Engineer", "company": "Example Labs", "bio": "Maya leads the real-time video team at Example Labs."}, "visible": false}
add_layer {"kind": "template", "template": "speaker-timer", "params": {"speaker": "Maya Chen", "duration": "20:00", "warn_at": 120}, "visible": false}
add_layer {"kind": "template", "template": "sponsor-carousel", "params": {"sponsors": [["Northwind Cloud", ""], ["Globex Labs", ""], ["Acme Robotics", ""]], "seconds": 5}, "visible": false}
add_layer {"kind": "template", "template": "agenda", "params": {"title": "Day 2", "items": ["Keynote", "Scaling video", "Panel: AI at work", "Lightning talks", "Closing"], "current": 1}, "visible": false}
add_layer {"kind": "template", "template": "question-card", "params": {"question": "How do you test latency at scale?", "source": "via Slido"}, "visible": false}
```

Speaker timers are usually seen by the speaker only: show them on the stream only if the organiser wants that.

## Steps

1. `status`, `start` if stopped; branded background, host webcam; `snapshot`.
2. Doors open (15-30 minutes): starting-soon with `until`, sponsor carousel in a corner. `save_scene {"name": "holding", "include_background": true, "include_webcam": true}`.
3. Opening: host on screen, agenda for a few seconds. Save as `"host"`.
4. Each talk: speaker-intro full frame for 10 seconds, then the speaker's slides (cutout_small, save as `"talk"`); `update_layer {"id": "speaker-timer", "params": {"speaker": "Jonas Weber", "duration": "15:00"}}` per speaker (restarts it).
5. Between sessions: `load_scene {"id": "holding", "mode": "replace"}`, `update_layer {"id": "agenda", "params": {"current": 3}}`.
6. Q&A and panels: question-card, one at a time; a lower third per panellist.
7. Close: thank-you card with the recording link, then `clear_layers`.

## Rules

- Speakers' names, titles and photos only as they provided them; attendees' names only with their consent.
- One moving element at a time: the sponsor carousel is off while a speaker talks.
- Graphics off faces and slides' text; 5% safe margin; check every change with `snapshot`.
- Keep the schedule: start each session's scene a minute early.
- Clear graphics at the end.
