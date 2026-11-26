# Reaction and watch-along streams

The content (a video, a game, a match) fills the picture, the streamer is cut out over it, and the stream goes to YouTube and/or Twitch at once. Page2Cam captures one application window with that app's sound, so nothing else on the screen is shown.

## Background

- `set_background {"full": "window", "window": "<id from list_windows>"}`: that one window, live, with its sound. Windows only.
- A browser tab with a video works too, but a browser's sound is everything playing in that browser (the engine says "Sound: everything playing in <app>"): the content must be in a second browser, with no call, music or chat tab in it.
- Games must run in borderless or windowed mode; fullscreen games cannot be captured.

## Webcam

- `set_overlay {"style": "cutout_full", "x": 0.85}`: the streamer cut out at the side, reacting; or `{"style": "cutout_small", "x": 0.95}` when the content needs the room.
- Keep the face off the part of the content people came to see.

## Graphics

```
add_layer {"kind": "template", "template": "starting-soon", "params": {"title": "Starting soon", "subtitle": "Watch-along with NovaPlays", "duration": "5:00"}}
add_layer {"kind": "template", "template": "be-right-back", "params": {"title": "Be right back", "subtitle": "Back in a few minutes", "duration": "3:00"}, "visible": false}
add_layer {"kind": "template", "template": "live-badge", "params": {"label": "LIVE"}, "visible": false}
```

Use the streamer's own name; add social handles only if they gave them. Starting-soon is a full-frame card: hide it before going live.

## Steps

1. `status`, `start` if stopped. Ask which window to share, then `list_windows` (only now: titles can be private).
2. `set_background {"full": "window", "window": …}` with the id of the one window the user named; if the answer lists several windows with that title, ask which. Read the note in the answer; `snapshot`.
3. `set_overlay {"style": "cutout_full", "x": 0.85}`; `snapshot`.
4. `set_stream_sites {"youtube": true, "twitch": true}` (only the sites the user wants).
5. `start_stream`. If it says a site has no stream key, tell the user to enter it in the control page; never ask for the key in the chat.
6. During the stream: `status` shows each site's state (a site can reconnect on its own). If the window is minimized or closed the note says so: ask the user to restore it.
7. End: `stop_stream`, then `clear_layers`.

## Rules

- The user listens to the content in their own browser or player, not through Page2Cam: the shared window's sound goes to the stream only, never into a call.
- Use a second browser for the content and never have a call in it: all of that browser's sound is streamed.
- Only rebroadcast content the user has the right to use; many platforms mute or remove streams of other people's videos, music or matches.
- Games in borderless or windowed mode.
- Say which window you are about to share before you choose it, and never pick between look-alike windows yourself.
- Twitch accepts up to 6000 kbps: if `status.stream_note` warns about it, the user lowers the video bitrate (`video_kbps` in the settings file).
- Never show chat usernames' personal details or private messages.
