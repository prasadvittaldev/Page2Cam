# Progress donut (`progress-donut`)

A ring that fills to value out of max, with the percentage (or your own text) in the centre and a label; changes animate.

Category: Data. Default place: x 0.78, y 0.08, w 0.17, h 0.3022 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `value` | number | `0` | Current amount, e.g. 68 |
| `max` | number | `100` | The full amount (100 for a percentage) |
| `label` | text | `""` | Line under the ring, e.g. Funded or Sprint done (empty: none) |
| `centre` | text | `""` | Text in the middle instead of the percentage, e.g. 34/50 (empty: the percentage) |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"label":"Sprint complete","max":100,"value":68},"template":"progress-donut"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"progress-donut","params":{"value":84}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
