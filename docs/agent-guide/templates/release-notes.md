# Release notes (`release-notes`)

"What's new" panel for a version: date and three optional lists (new, improved, fixed) under coloured labels.

Category: Software companies. Default place: x 0.58, y 0.08, w 0.37, h 0.72 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `version` | text | `""` | **required.** Version, e.g. v2.4.0 |
| `title` | text | `"What's new"` | Small heading above the version |
| `date` | text | `""` | Release date as shown, e.g. 24 Nov 2026 (empty: none) |
| `new` | list | `[]` | New features, one short line each (empty: section hidden) |
| `improved` | list | `[]` | Improvements, one short line each (empty: section hidden) |
| `fixed` | list | `[]` | Fixes, one short line each (empty: section hidden). About 12 lines fit in all |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"date":"24 Nov 2026","fixed":["Webcam reconnects after sleep"],"improved":["Faster cut-out on laptop GPUs","Smoother tickers at 30 fps"],"new":["Overlay templates with live parameters","Scenes: switch several layers at once"],"version":"v2.4.0"},"template":"release-notes"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"release-notes","params":{"fixed":["Webcam reconnects after sleep","Stream key no longer asked twice"]}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
