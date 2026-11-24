# Countdown (`countdown`)

Counts down to a time of day or date, or for a duration from when it is shown; shows a message at zero.

Category: Time. Default place: x 0.72, y 0.05, w 0.23, h 0.16 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `label` | text | `"Starting in"` | Text above the time |
| `until` | datetime | `""` | Count to this time: 18:30 (today or the next one) or 2026-12-31T18:30. Wins over duration |
| `duration` | duration | `300` | Or count down this long (seconds or m:ss), from when the layer is shown or the value changes |
| `done_text` | text | `"Starting now"` | Shown at zero |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"duration":"5:00","label":"We start in"},"template":"countdown"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"countdown","params":{"duration":"2:00","label":"Back in"}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
