# Confidential badge (`confidential-badge`)

A bordered CONFIDENTIAL (or any) warning badge for the top right, with an optional second line.

Category: Basics. Default place: x 0.75, y 0.05, w 0.22, h 0.1 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `text` | text | `"CONFIDENTIAL"` | The badge word, e.g. CONFIDENTIAL, INTERNAL, DRAFT, EMBARGOED |
| `note` | text | `""` | Small second line, e.g. Internal only, do not record (empty: none) |
| `colour` | choice | `"red"` | Badge colour: red, amber, or the look's accent Options: red, amber, accent. |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"note":"Internal only, do not record","text":"CONFIDENTIAL"},"template":"confidential-badge"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"confidential-badge","params":{"colour":"amber","note":"Figures not final","text":"DRAFT"}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
