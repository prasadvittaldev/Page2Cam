# Scoreboard (`scoreboard`)

A score bug for two teams or players: names, team colours, scores, the period and an optional clock text; a changed score animates with a soft glow.

Category: Data. Default place: x 0.05, y 0.05, w 0.4, h 0.1 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `home_name` | text | `""` | **required.** First (left) team or player, short, e.g. LIONS or Team A |
| `home_score` | number | `0` | Left score |
| `home_colour` | colour | `""` | Left team colour #rrggbb (empty: the accent colour) |
| `away_name` | text | `""` | **required.** Second (right) team or player, short |
| `away_score` | number | `0` | Right score |
| `away_colour` | colour | `""` | Right team colour #rrggbb (empty: grey) |
| `period` | text | `""` | Period text, e.g. Q3, 2nd half, Round 4 (empty: none) |
| `clock` | text | `""` | Clock text as shown, e.g. 67:12 (it does not run by itself; update it or leave empty) |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"away_colour":"#1fa37a","away_name":"Eagles","away_score":1,"clock":"67:12","home_colour":"#f2b705","home_name":"Lions","home_score":2,"period":"2nd half"},"template":"scoreboard"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"scoreboard","params":{"clock":"71:40","home_score":3}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
