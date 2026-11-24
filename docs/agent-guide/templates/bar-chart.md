# Bar chart (`bar-chart`)

Labelled bars from a table of label/value rows; changing values animates the bars.

Category: Data. Default place: x 0.55, y 0.08, w 0.4, h 0.5 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `title` | text | `""` | Heading (empty: none) |
| `rows` | table | `[]` | **required.** Up to 12 rows: label, value (a number) Rows of [label, value]. |
| `unit` | text | `""` | Shown after each value, e.g. % or k |
| `max` | number | `0` | Top of the scale (0: the largest value) |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"rows":[["Mon","120"],["Tue","180"],["Wed","150"],["Thu","240"],["Fri","210"]],"title":"Signups this week"},"template":"bar-chart"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"bar-chart","params":{"rows":[["Mon","120"],["Tue","180"],["Wed","150"],["Thu","240"],["Fri","320"]]}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
