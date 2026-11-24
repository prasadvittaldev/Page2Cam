# Goal bar (`goal-bar`)

A fundraising or follower goal: a bar that fills towards the goal, the amount raised and "62% of $10,000"; raising the amount animates the bar.

Category: Data. Default place: x 0.05, y 0.08, w 0.4, h 0.14 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `title` | text | `"Our goal"` | Heading, e.g. Charity stream or Road to 10k subscribers |
| `current` | number | `0` | Amount reached so far (a number, no currency sign) |
| `goal` | number | `1000` | The target amount (a number) |
| `prefix` | text | `""` | Before amounts, e.g. $ or € (empty: none) |
| `unit` | text | `""` | After amounts, e.g. " followers" (include a leading space for words) |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"current":6200,"goal":10000,"prefix":"$","title":"Charity stream"},"template":"goal-bar"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"goal-bar","params":{"current":7450}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
