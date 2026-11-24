# Speaker timer (`speaker-timer`)

A per-speaker countdown: turns amber near the end, red at zero, then counts the overtime as +m:ss.

Category: Time. Default place: x 0.78, y 0.05, w 0.19, h 0.16 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `speaker` | text | `""` | **required.** Who is speaking now. Changing it restarts the timer |
| `duration` | duration | `300` | Speaking time (seconds or m:ss, e.g. 5:00). Changing it restarts the timer |
| `warn_at` | number | `60` | Turn amber when this many seconds are left 0 to 3600. |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"duration":"5:00","speaker":"Dana Lee","warn_at":60},"template":"speaker-timer"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"speaker-timer","params":{"duration":"3:00","speaker":"Sam Ortiz"}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
