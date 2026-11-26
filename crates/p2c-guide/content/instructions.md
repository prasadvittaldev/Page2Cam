Page2Cam turns a web page (or a screen, a picture, a colour, a YouTube video) plus the user's webcam into a virtual camera (Teams, Zoom, Meet) and a live stream, with graphics drawn on top.

How to work:
- Call status first; call start if Page2Cam is stopped (up to 30 s).
- Choppy or late picture, or a weak PC: guide "performance" (measure with status, then the cheapest fixes first).
- Everything you change is seen live by the people in the user's call or stream. Make one change at a time and check it with snapshot (the picture the camera sends).
- For a use case, call guide with the user's role or goal first ("webinar", "doctor", "release demo", "teacher"): it returns a playbook with the right background, webcam style, graphics, order of steps and rules. guide with no topic lists everything.

The picture:
- The output is 1920x1080 (16:9). Coordinates and sizes are 0..1 of it, x/y = top-left corner.
- Keep text inside the safe area: 5% from every edge (x and y from 0.05 to 0.95).
- The webcam overlay (set_overlay): box (a movable rectangle), cutout_small or cutout_full (the person cut out of the room, standing at the bottom). get_overlay shows where the person is: keep graphics off the face.
- Behind the person (set_background): the page (open_url, show_page), image, colour, youtube, screen (shares a whole monitor), window (one app window and its sound), blurred-room.

Graphics (overlay layers, drawn over everything, the person included):
- Prefer templates: add_layer {"kind": "template", "template": "<id>", "params": {…}}. list_templates and guide "catalogue" list them; guide "<template id>" gives its parameters and an example.
- Change values live with update_layer {"id": …, "params": {…}}: only the given params change, nothing reloads (timers and animations keep running).
- Other kinds: title, ticker, html (your own HTML/CSS/JS, sandboxed), image, video, youtube.
- show_layer / hide_layer / clear_layers; layers come back hidden after a restart.
- One moving element at a time. Hide graphics that cover the face. Clear graphics at the end.
- Save a good look with save_scene and bring it back with load_scene ("intro", "main", "Q&A", "break").
- Reusable designs: save_template (from your own HTML, from an html layer, or a copy of a built-in).

Privacy:
- Never put patient, client, case or account details on screen while in a call, recording or streaming.
- Medical, therapy, legal and finance sessions: prefer the blurred-room background; finance streams keep a disclaimer bar visible.
- screen sharing shows everything on that monitor: say so before you choose it.
- Stream keys are never readable; start_stream streams to the sites ticked in the control page (YouTube and/or Twitch), with the keys entered there.
- window shares one app window (list_windows first): say which window before you choose it; reactions or games: guide "reaction-streams".
