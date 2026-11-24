# Callout (`callout`)

An arrow with a label that points at a spot on a shared screen or demo, with an optional softly pulsing ring at the tip.

Category: Software companies. Default place: x 0.55, y 0.44, w 0.24, h 0.1 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `label` | text | `""` | **required.** Short label, e.g. Click Publish |
| `direction` | choice | `"left"` | Where the arrow points; its tip sits near that edge of the layer box, so place the box next to the spot. For up/down use a taller box (e.g. w 0.14, h 0.25) Options: left, right, up, down. |
| `pulse` | bool | `true` | Softly pulsing ring at the arrow tip |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"direction":"left","label":"Click Publish"},"template":"callout"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"callout","params":{"label":"Then pick a channel"}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
