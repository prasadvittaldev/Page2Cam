---
title: Saved looks (scenes)
aliases: scene, look, saved look, preset, intro outro, switch look
---
# Saved looks (scenes)

A scene is the current set of graphics (every layer, shown or hidden), optionally with the background and the webcam overlay, saved under a name. Typical scenes: "intro" (starting-soon card), "main" (lower third, logo, agenda), "q-and-a" (question card), "break" (be-right-back), "outro" (thank-you).

- Save: `save_scene {"name": "Main", "include_background": true, "include_webcam": true}`. Saving again with the same name replaces it. Pictures and videos are copied, so the scene keeps working after its layers are removed.
- List: `list_scenes`.
- Load: `load_scene {"id": "main", "mode": "replace"}` removes the other layers first; `"mode": "add"` puts the scene's layers in front of the current ones (with new ids where needed). Layers show as saved (shown or hidden). A saved background and webcam overlay are restored too.
- Delete: `delete_scene {"id": "main"}`.

Loading a scene whose template or picture no longer exists loads the rest and says what was skipped.

Build the scenes before the show (prepare layers with `visible: false`, then save), then switch between them with one call each during the show. Check every switch with `snapshot`.
