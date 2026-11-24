# Status card (`status-card`)

Service status panel: a coloured overall status, the incident title, a timeline of updates (newest first) and optional per-component states.

Category: Software companies. Default place: x 0.64, y 0.08, w 0.31, h 0.66 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `service` | text | `""` | Service or product name above the status (empty: none) |
| `status` | choice | `"operational"` | Overall status: green operational, amber degraded, orange partial-outage, red major-outage, blue maintenance Options: operational, degraded, partial-outage, major-outage, maintenance. |
| `headline` | text | `""` | Status line text (empty: from the status, e.g. All systems operational) |
| `incident` | text | `""` | Incident title (empty: none) |
| `updates` | table | `[]` | Up to 6 rows, newest first: time (e.g. 14:32), text Rows of [time, text]. |
| `components` | table | `[]` | Up to 8 rows: component name, status (operational, degraded, partial-outage, major-outage or maintenance; other text is shown grey) Rows of [name, status]. |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"components":[["API","operational"],["Checkout (EU)","degraded"],["Dashboard","operational"],["Webhooks","operational"]],"incident":"Elevated error rates on EU checkout","service":"Acme API","status":"degraded","updates":[["14:32","Fix deployed, monitoring recovery."],["14:05","Cause identified: a faulty cache node."],["13:48","Investigating increased errors on EU checkout."]]},"template":"status-card"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"status-card","params":{"components":[["API","operational"],["Checkout (EU)","operational"],["Dashboard","operational"],["Webhooks","operational"]],"status":"operational","updates":[["14:55","Resolved: error rates are back to normal."],["14:32","Fix deployed, monitoring recovery."],["14:05","Cause identified: a faulty cache node."],["13:48","Investigating increased errors on EU checkout."]]}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
