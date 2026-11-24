# Clock (`clock`)

The current time, local or at a fixed UTC offset, 24h or 12h, with an optional label such as a city.

Category: Time. Default place: x 0.82, y 0.05, w 0.15, h 0.1 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `label` | text | `""` | Small line above the time, e.g. London (empty: none) |
| `format` | choice | `"24h"` | 24h (18:30) or 12h (6:30 PM) Options: 24h, 12h. |
| `seconds` | bool | `false` | Show seconds |
| `use_local` | bool | `true` | true: this computer's time zone; false: use utc_offset |
| `utc_offset` | number | `0` | Hours from UTC when use_local is false, e.g. -5, 1, 5.5 (no daylight saving: give the current offset) -12 to 14. |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"format":"24h","label":"Local time","seconds":true},"template":"clock"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"clock","params":{"format":"12h","label":"New York","use_local":false,"utc_offset":-5}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
