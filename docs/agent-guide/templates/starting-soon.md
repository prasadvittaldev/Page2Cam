# Starting soon (`starting-soon`)

Full-frame card before the show: title, subtitle and a countdown.

Category: Full-frame cards. Default place: covers the whole output (a full-frame card).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `title` | text | `"Starting soon"` | Big heading |
| `subtitle` | text | `""` | Line under it, e.g. the show's name |
| `until` | datetime | `""` | Count down to this time (18:30 or 2026-12-31T18:30) |
| `duration` | duration | `0` | Or count down this long from showing (0 and no time: no countdown) |
| `background` | colour | `""` | Card colour (empty: the look's) |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"duration":"3:00","subtitle":"Product launch: what is new in 2.0","title":"Starting soon"},"template":"starting-soon"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"starting-soon","params":{"duration":"1:00","subtitle":"Grab a coffee, we start in a minute"}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
