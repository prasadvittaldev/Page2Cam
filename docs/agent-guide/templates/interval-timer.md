# Interval timer (`interval-timer`)

Fitness intervals: WORK and REST phases for a number of rounds, with the round, the time left and a phase colour; shows Done at the end.

Category: Time. Default place: x 0.78, y 0.05, w 0.19, h 0.2 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `work` | duration | `40` | Work time per round (seconds or m:ss) |
| `rest` | duration | `20` | Rest time between rounds (seconds or m:ss; 0: none) |
| `rounds` | number | `8` | Number of rounds 1 to 99. |
| `label` | text | `""` | Small line at the top, e.g. the workout's name (empty: none) |
| `done_text` | text | `"Done"` | Shown when all rounds are over |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"label":"HIIT","rest":20,"rounds":8,"work":40},"template":"interval-timer"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"interval-timer","params":{"label":"Core","rounds":6,"work":30}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
