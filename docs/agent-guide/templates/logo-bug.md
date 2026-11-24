# Logo bug (`logo-bug`)

A logo or small picture in a corner, slightly see-through like a TV channel bug.

Category: Basics. Default place: x 0.86, y 0.05, w 0.11, h 0.1956 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `image` | asset | `""` | **required.** The logo: an https address or a data:image URI (PNG with transparency or SVG looks best) |
| `opacity` | number | `0.85` | How solid the logo is: 0.2 (faint) to 1 (solid) 0.2 to 1. |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"image":"data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHZpZXdCb3g9IjAgMCAxMDAgMTAwIj48cmVjdCB4PSI0IiB5PSI0IiB3aWR0aD0iOTIiIGhlaWdodD0iOTIiIHJ4PSIyMiIgZmlsbD0iIzBiNjNmMCIvPjxjaXJjbGUgY3g9IjUwIiBjeT0iNTAiIHI9IjI3IiBmaWxsPSJub25lIiBzdHJva2U9IiNmZmZmZmYiIHN0cm9rZS13aWR0aD0iOCIvPjxjaXJjbGUgY3g9IjUwIiBjeT0iNTAiIHI9IjEwIiBmaWxsPSIjZmZmZmZmIi8+PC9zdmc+","opacity":0.9},"template":"logo-bug"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"logo-bug","params":{"opacity":0.4}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
