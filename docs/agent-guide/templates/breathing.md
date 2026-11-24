# Breathing guide (`breathing`)

A calm circle that slowly grows while breathing in, rests while holding and shrinks while breathing out, with the phase written inside.

Category: Calm. Default place: x 0.35, y 0.2333, w 0.3, h 0.5333 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `title` | text | `""` | Line above the circle, e.g. Breathe with me (empty: none) |
| `inhale` | number | `4` | Seconds breathing in 1 to 20. |
| `hold` | number | `4` | Seconds holding after breathing in (0: no hold) 0 to 20. |
| `exhale` | number | `6` | Seconds breathing out 1 to 20. |
| `hold_after` | number | `0` | Seconds resting after breathing out (0: none) 0 to 20. |
| `cycles` | number | `0` | Breaths to guide, then the done text (0: endless). Changing a timing or this restarts the count 0 to 100. |
| `inhale_text` | text | `"Breathe in"` | Text while breathing in |
| `hold_text` | text | `"Hold"` | Text while holding |
| `exhale_text` | text | `"Breathe out"` | Text while breathing out |
| `done_text` | text | `"Well done"` | Text after the last breath |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"cycles":5,"title":"Breathe with me"},"template":"breathing"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"breathing","params":{"title":"One more minute, slowly"}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
