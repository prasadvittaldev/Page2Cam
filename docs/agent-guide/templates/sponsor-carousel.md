# Sponsor carousel (`sponsor-carousel`)

Thanks sponsors one at a time: each logo (or name) fades in, stays a few seconds, then gives way to the next.

Category: Call to action. Default place: x 0.72, y 0.7, w 0.23, h 0.22 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `heading` | text | `"Thanks to our sponsors"` | Line above the sponsor (empty: none) |
| `sponsors` | table | `[]` | **required.** Up to 20 rows: name, logo (an https address or a data:image URI; empty: the name is shown large instead) Rows of [name, logo]. |
| `seconds` | number | `5` | Seconds each sponsor stays 2 to 60. |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"heading":"Thanks to our sponsors","seconds":5,"sponsors":[["Northwind Cloud",""],["Globex Labs",""],["Acme Robotics",""]]},"template":"sponsor-carousel"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"sponsor-carousel","params":{"heading":"Powered by","sponsors":[["Northwind Cloud",""],["Globex Labs",""],["Acme Robotics",""],["Initech",""]]}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
