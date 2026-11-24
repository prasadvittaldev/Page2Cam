# Lower third (`lower-third`)

Name and role (or any two lines) in a band at the bottom left, with an optional logo.

Category: Basics. Default place: x 0.05, y 0.74, w 0.5, h 0.15 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `title` | text | `""` | **required.** Main line, e.g. the person's name |
| `subtitle` | text | `""` | Second line, e.g. role and company (empty: one line) |
| `logo` | asset | `""` | Small picture at the left: an https address or a data:image URI |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"subtitle":"Head of Product, Example Co","title":"Alex Kim"},"template":"lower-third"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"lower-third","params":{"subtitle":"Founder, Example Co"}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
