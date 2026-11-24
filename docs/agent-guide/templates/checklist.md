# Checklist (`checklist`)

A checklist with ticked and open items and a done count; tick items off live (release checklist, preflight, to-do list).

Category: Lists and text. Default place: x 0.66, y 0.1, w 0.29, h 0.5 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `title` | text | `"Checklist"` | Heading (empty: none) |
| `items` | table | `[]` | **required.** Up to 12 rows: item text, done (yes/no or 1/0) Rows of [item, done]. |
| `show_count` | bool | `true` | Show the done count (e.g. 2 / 5) next to the heading |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"items":[["Tests green","yes"],["Docs updated","yes"],["Changelog written","no"],["Release notes sent","no"],["Tag the release","no"]],"title":"Launch checklist"},"template":"checklist"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"checklist","params":{"items":[["Tests green","yes"],["Docs updated","yes"],["Changelog written","yes"],["Release notes sent","no"],["Tag the release","no"]]}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
