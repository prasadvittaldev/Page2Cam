# Architecture diagram (`architecture-diagram`)

Boxes and arrows laid out automatically: nodes in numbered columns from left to right, edges as arrows with optional labels.

Category: Software companies. Default place: x 0.45, y 0.08, w 0.5, h 0.6 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `title` | text | `""` | Heading (empty: none) |
| `nodes` | table | `[]` | **required.** Up to 24 rows: id (short, unique), label (shown), column 1 to 6 (left to right; nodes of a column are spaced evenly top to bottom in row order) Rows of [id, label, column]. |
| `edges` | table | `[]` | Up to 40 arrows: from id, to id, label (optional, keep it short). Unknown ids are ignored Rows of [from, to, label]. |
| `highlight` | text | `""` | Id of a node to fill with the accent colour, e.g. the one being discussed (empty: none) |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"edges":[["web","gw",""],["mobile","gw",""],["gw","auth","JWT"],["gw","orders","REST"],["orders","db","SQL"],["orders","queue","events"]],"highlight":"orders","nodes":[["web","Web app",1],["mobile","Mobile app",1],["gw","API gateway",2],["auth","Auth service",3],["orders","Orders service",3],["db","Postgres",4],["queue","Event queue",4]],"title":"Checkout architecture"},"template":"architecture-diagram"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"architecture-diagram","params":{"edges":[["web","gw",""],["mobile","gw",""],["gw","auth","JWT"],["gw","orders","REST"],["orders","cache","reads"],["orders","db","SQL"],["orders","queue","events"]],"highlight":"cache","nodes":[["web","Web app",1],["mobile","Mobile app",1],["gw","API gateway",2],["auth","Auth service",3],["orders","Orders service",3],["cache","Redis cache",4],["db","Postgres",4],["queue","Event queue",4]]}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
