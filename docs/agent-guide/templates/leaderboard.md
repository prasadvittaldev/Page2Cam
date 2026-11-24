# Leaderboard (`leaderboard`)

Ranked names and scores, sorted highest first, with gold, silver and bronze for the top three; changed scores count up and the order follows.

Category: Data. Default place: x 0.66, y 0.08, w 0.29, h 0.6 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `title` | text | `"Leaderboard"` | Heading (empty: none) |
| `rows` | table | `[]` | **required.** Up to 10 rows: name, score (a number). Sorted by score, highest first; send the whole table on every change Rows of [name, score]. |
| `unit` | text | `""` | Shown after each score, e.g. " pts" |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"rows":[["Maya","1280"],["Jonas","1140"],["Priya","980"],["Leo","860"],["Sam","720"]],"title":"Top contributors","unit":" pts"},"template":"leaderboard"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"leaderboard","params":{"rows":[["Maya","1280"],["Jonas","1140"],["Priya","980"],["Leo","1350"],["Sam","760"]]}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
