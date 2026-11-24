# Churches and faith groups

A reverent, readable broadcast of a service, sermon or study: the speaker clear, songs and readings on screen so people at home can join in, and gentle graphics that never draw attention to themselves.

## Background

- Streaming a service with a room camera: the camera is the picture; the webcam as a large `box` or the page showing the room camera if available.
- A sermon or study from home or an office: `blurred-room`, or the church's image (`upload_background_image`, `{"full": "image"}`).
- Slides or a song projection page with `open_url`; `screen` only on a monitor that shows nothing else (it shares the whole monitor).

## Webcam

- Speaker or preacher: `set_overlay {"style": "cutout_full", "x": 0.35}`, text on the right or below.
- Worship and music: `{"style": "box", "x": 0.5, "y": 0.4, "width": 0.8}`, lyrics in the lower part.

## Graphics

```
add_layer {"kind": "template", "template": "starting-soon", "params": {"title": "Sunday service", "subtitle": "Grace Community Church", "until": "10:30"}}
add_layer {"kind": "template", "template": "lyrics-card", "params": {"lines": "Amazing grace, how sweet the sound\nThat saved a wretch like me", "reference": "Amazing Grace, verse 1"}, "visible": false}
add_layer {"kind": "template", "template": "lyrics-card", "id": "reading", "params": {"lines": "For God so loved the world that he gave his one and only Son", "reference": "John 3:16"}, "visible": false}
add_layer {"kind": "template", "template": "lower-third", "params": {"title": "Pastor James Allen", "subtitle": "Grace Community Church"}, "hide_after_s": 15, "visible": false}
add_layer {"kind": "template", "template": "qr-card", "params": {"link": "https://example.org/give", "title": "Give online", "caption": "example.org/give"}, "visible": false}
add_layer {"kind": "template", "template": "goal-bar", "params": {"title": "Roof appeal", "current": 18400, "goal": 30000, "prefix": "$"}, "visible": false}
```

Song lyrics only where the church holds a streaming licence for them; scripture with its reference.

## Steps

1. `status`, `start` if stopped; background and webcam; `snapshot`.
2. 15 minutes before: starting-soon counting to the start time. `save_scene {"name": "welcome", "include_background": true, "include_webcam": true}`.
3. Welcome: hide it; lower third for the leader.
4. Worship: `load_scene {"id": "worship", "mode": "replace"}` (saved once with the box look); lyrics-card replaced verse by verse a line ahead: `update_layer {"id": "lyrics-card", "params": {"lines": "I once was lost, but now am found\nWas blind, but now I see"}}`. Hide between songs.
5. Readings and sermon: `load_scene {"id": "sermon", "mode": "replace"}` (saved once with the speaker look); show the reading card while the passage is read, then hide it.
6. Offering or appeal: qr-card for giving, goal-bar with the real total (`update_layer {"id": "goal-bar", "params": {"current": 19650}}`).
7. Blessing and close: thank-you card with the next service time, then `clear_layers`.

## Rules

- Never show names of people prayed for, pastoral or health details, or children's names and faces without consent.
- Lyrics and readings: large, few lines, nothing moving at the same time.
- No tickers or flashy animation during prayer, readings or sermon.
- Keep faces clear; 5% safe margin; check every change with `snapshot`.
- Clear graphics at the end.
