# KPI cards (`kpi-cards`)

2 to 4 KPI tiles with a big value and a green/red change arrow; also the metrics board for software companies (MRR, DAU, uptime, latency). Numeric values animate on change.

Category: Data. Default place: x 0.05, y 0.06, w 0.6, h 0.2 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `kpis` | table | `[]` | **required.** 1 to 4 rows: label, value (e.g. $48.2k, 99.98%, 182 ms), change (e.g. +12% or -3%; empty: none), note (e.g. vs last week; optional) Rows of [label, value, change, note]. |
| `title` | text | `""` | Small heading above the tiles (empty: none) |
| `invert` | text | `""` | Labels (comma-separated) where going down is good, e.g. p95 latency, Churn: their arrows are green when negative |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"invert":"p95 latency","kpis":[["MRR","$48.2k","+6.1%","vs last month"],["Daily users","12,480","+12%","vs last week"],["Uptime","99.98%","+0.01%","30 days"],["p95 latency","182 ms","-9%","vs last week"]]},"template":"kpi-cards"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"kpi-cards","params":{"kpis":[["MRR","$49.6k","+9.0%","vs last month"],["Daily users","12,910","+15%","vs last week"],["Uptime","99.98%","+0.01%","30 days"],["p95 latency","176 ms","-12%","vs last week"]]}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
