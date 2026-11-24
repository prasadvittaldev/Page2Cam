---
title: Musicians and performers
group: creators
aliases: musician, singer, band, performer, dj, concert, gig, songwriter, guitarist, pianist, live music, recital
prompt: Set up my live performance
args: name, event
---
# Musicians and performers

A stage-like picture: the performer large and in focus, the song on screen, lyrics only when they help, and very little else. The music is the show; graphics stay small and still.

## Background

- `set_background {"full": "colour", "full_colour": "#0d0d14"}` with `refined_edges` true: a dark stage behind a cut-out performer.
- Or `blurred-room` for a cosy home-studio feel; or an image of the venue or album art (`upload_background_image`, `{"full": "image"}`).
- A visualiser or animated page (`open_url`) only if the machine keeps up (check `status` for skipped frames).

## Webcam

- `set_overlay {"style": "cutout_full", "x": 0.5}`: performer centred, full height.
- Instruments wider than the person (keyboard, drum kit) get cut badly: use `{"style": "box", "x": 0.5, "width": 0.8}` with `in_box` room instead.

## Graphics

```
add_layer {"kind": "template", "template": "lower-third", "params": {"title": "Lena Hart", "subtitle": "Live from the living room"}, "hide_after_s": 15}
add_layer {"kind": "template", "template": "now-playing", "params": {"title": "Northern Lights", "artist": "Lena Hart"}, "visible": false}
add_layer {"kind": "template", "template": "lyrics-card", "params": {"lines": "We were running out of daylight\nBut you held the sky in place", "reference": "Northern Lights"}, "visible": false}
add_layer {"kind": "template", "template": "qr-card", "params": {"link": "https://example.com/tip", "title": "Support the music", "caption": "example.com/tip"}, "visible": false}
add_layer {"kind": "template", "template": "be-right-back", "params": {"title": "Short break", "subtitle": "Tuning up, back in a moment", "duration": "2:00"}, "visible": false}
```

Lyrics only for songs the user wrote or has the rights to display.

## Steps

1. `status`, `start` if stopped; `set_audio {"mic_on": true, "page_on": false}` so only the performance is heard; background, webcam, `snapshot`.
2. Opening: lower third with the performer's name (hides itself).
3. Each song: `update_layer {"id": "now-playing", "params": {"title": "Paper Boats", "artist": "Lena Hart"}}`, show it for the first 20 seconds, then hide it.
4. Sing-along: lyrics-card, replaced verse by verse with `update_layer {"id": "lyrics-card", "params": {"lines": "..."}}` a line or two ahead of the singer. Hide it for instrumental parts.
5. Between sets: be-right-back (mic muted by the user), then `load_scene {"id": "stage", "mode": "replace"}` (save it once with `save_scene {"name": "stage", "include_background": true, "include_webcam": true}`).
6. Encore / close: qr-card for tips or merch for a minute, then `clear_layers`.

## Rules

- Keep the performer's face and hands (instrument) clear of graphics; check with `snapshot`.
- One element at a time: never lyrics and now-playing together.
- No tickers or busy animation during a song.
- 5% safe margin for text.
- Clear graphics at the end.
