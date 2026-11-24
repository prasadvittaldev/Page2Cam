# Stopwatch (`stopwatch`)

Counts up from when it is shown; can be paused and resumed without losing the time, or restarted from a given offset.

Category: Time. Default place: x 0.82, y 0.05, w 0.15, h 0.12 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `label` | text | `"Elapsed"` | Small line above the time (empty: none) |
| `running` | bool | `true` | false pauses (the time is kept), true resumes |
| `start_at` | duration | `0` | Start from this time (seconds or m:ss). Changing it restarts the stopwatch from that value |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"label":"Elapsed","start_at":0},"template":"stopwatch"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"stopwatch","params":{"label":"Q&A","start_at":"10:00"}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
