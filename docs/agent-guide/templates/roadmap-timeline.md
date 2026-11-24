# Roadmap timeline (`roadmap-timeline`)

Milestones on a horizontal timeline: done ones filled, the current one highlighted, next and later ones outlined.

Category: Software companies. Default place: x 0.05, y 0.72, w 0.9, h 0.2 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `title` | text | `"Roadmap"` | Heading (empty: none) |
| `milestones` | table | `[]` | **required.** Up to 8 rows, left to right: label, when (e.g. Q1 or Mar), status: done, now, next or later Rows of [label, when, status]. |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"milestones":[["Private beta","Sep","done"],["Public beta","Nov","done"],["v1.0 launch","Jan","now"],["Teams app","Mar","next"],["Enterprise SSO","Q2","later"]],"title":"Roadmap 2027"},"template":"roadmap-timeline"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"roadmap-timeline","params":{"milestones":[["Private beta","Sep","done"],["Public beta","Nov","done"],["v1.0 launch","Jan","done"],["Teams app","Mar","now"],["Enterprise SSO","Q2","next"]]}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
