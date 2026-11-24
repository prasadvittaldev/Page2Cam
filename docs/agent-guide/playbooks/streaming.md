# Game and live streamers

A lively but readable stream: the game or content fills the picture, the streamer is cut out in a corner, and a few graphics (live badge, handles, a goal or score) frame it without hiding the action.

## Background

- `set_background {"full": "screen", "screen": "DISPLAY1"}` for a game or desktop on a monitor (`list_screens` first). It shares the WHOLE monitor: tell the user to turn on Do Not Disturb, close chats, password managers, email and private windows on it before you switch.
- Or a browser game, a web overlay or a playlist page with `open_url`, or a YouTube video with `{"full": "youtube", "youtube": "<link>"}` for watch-alongs (only content the user has the rights to stream).

## Webcam

- `set_overlay {"style": "cutout_small", "x": 0.95}`: the streamer small in the bottom right, standing on the edge. Move to `"x": 0.05` if the game's map or HUD sits there.
- For a just-chatting segment: `{"style": "cutout_full", "x": 0.5}` over a page or colour.

## Graphics

```
add_layer {"kind": "template", "template": "starting-soon", "params": {"title": "Starting soon", "subtitle": "Ranked grind with NovaPlays", "duration": "5:00"}}
add_layer {"kind": "template", "template": "live-badge", "params": {"label": "LIVE"}, "visible": false}
add_layer {"kind": "template", "template": "social-handles", "params": {"handles": [["Twitch", "novaplays"], ["YouTube", "@novaplays"], ["Discord", "discord.gg/nova"]], "layout": "row"}, "x": 0.05, "y": 0.86, "w": 0.5, "h": 0.08, "visible": false}
add_layer {"kind": "template", "template": "goal-bar", "params": {"title": "Road to 10k followers", "current": 8240, "goal": 10000, "unit": " followers"}, "visible": false}
add_layer {"kind": "template", "template": "scoreboard", "params": {"home_name": "Nova", "home_score": 0, "away_name": "Chat", "away_score": 0, "period": "Round 1"}, "visible": false}
add_layer {"kind": "template", "template": "be-right-back", "params": {"title": "Be right back", "subtitle": "Grabbing a drink", "duration": "3:00"}, "visible": false}
```

Use the streamer's own name and handles; leave out handles they did not give.

## Steps

1. `status`, `start` if stopped; background and webcam as above; `snapshot`.
2. Intro (5 minutes before): starting-soon full frame with its countdown. `save_scene {"name": "intro", "include_background": true, "include_webcam": true}`.
3. Go live: `hide_layer {"id": "starting-soon"}`, show live-badge and social-handles (`hide_layer {"id": "social-handles"}` after a minute; show it again every 20-30 minutes). `save_scene {"name": "main", "include_background": true, "include_webcam": true}`.
4. During play: one data graphic at a time, in a corner the game does not use. Score changes: `update_layer {"id": "scoreboard", "params": {"home_score": 3, "period": "Round 4"}}`. Follower goal: `update_layer {"id": "goal-bar", "params": {"current": 8410}}`.
5. Breaks: `show_layer {"id": "be-right-back"}` (covers everything, mutes nothing: tell the user to mute the mic with `set_audio {"mic_on": false}`). Back: hide it, `load_scene {"id": "main", "mode": "replace"}`.
6. Chat Q&A / just chatting: switch to `cutout_full` over a colour or page; question-card for a chat question if the user wants.
7. End: thank-you card with handles, then `clear_layers`, `stop_stream` if streaming.

## Rules

- `screen` shows everything on that monitor, notifications included: warn before choosing it, and prefer a dedicated game monitor.
- Never show chat usernames' personal details, emails or anything from private messages.
- One moving element at a time: no scoreboard glow next to an animating goal bar.
- Keep HUD areas (minimap, health, ammo) and the face clear; check with `snapshot` after every change.
- Keep text inside the 5% safe margin; the platform covers edges with its own player controls.
- Clear graphics at the end (`clear_layers`).
