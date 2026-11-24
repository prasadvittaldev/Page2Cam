# Line chart (`line-chart`)

A line chart with a shaded area from label/value rows, the latest value shown big; changed values animate.

Category: Data. Default place: x 0.55, y 0.08, w 0.4, h 0.45 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `title` | text | `""` | Heading (empty: none) |
| `points` | table | `[]` | **required.** 2 to 60 rows in time order: label (e.g. a day or month), value (a number). Rows whose value is not a number are skipped Rows of [label, value]. |
| `unit` | text | `""` | Shown after the latest value, e.g. % or k or ms |
| `from_zero` | bool | `false` | Start the scale at 0 instead of just below the smallest value |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"points":[["Mon","8.2"],["Tue","9.1"],["Wed","8.7"],["Thu","10.4"],["Fri","11.0"],["Sat","9.6"],["Sun","12.4"]],"title":"Daily active users","unit":"k"},"template":"line-chart"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"line-chart","params":{"points":[["Tue","9.1"],["Wed","8.7"],["Thu","10.4"],["Fri","11.0"],["Sat","9.6"],["Sun","12.4"],["Mon","14.1"]]}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
