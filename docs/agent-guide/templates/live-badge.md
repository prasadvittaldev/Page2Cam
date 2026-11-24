# Live badge (`live-badge`)

A LIVE, REC or ON AIR pill with a slowly pulsing dot, for a top corner.

Category: Basics. Default place: x 0.03, y 0.05, w 0.1, h 0.06 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `label` | choice | `"LIVE"` | Which word to show Options: LIVE, REC, ON AIR. |
| `text` | text | `""` | Custom text instead of the label, e.g. LIVE FROM BERLIN (empty: the label). Keep it short |
| `colour` | colour | `""` | Dot colour #rrggbb (empty: red) |
| `pulse` | bool | `true` | Let the dot pulse slowly |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"label":"LIVE"},"template":"live-badge"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"live-badge","params":{"label":"ON AIR"}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
