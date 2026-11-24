# Agenda (`agenda`)

A numbered agenda with the current item highlighted and earlier items ticked; move `current` as the session goes on.

Category: Lists and text. Default place: x 0.66, y 0.1, w 0.29, h 0.5 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `title` | text | `"Agenda"` | Heading (empty: none) |
| `items` | list | `[]` | **required.** Up to 12 agenda items, in order, short (a few words each) |
| `current` | number | `0` | The item happening now, 1-based (0: none highlighted). Earlier items are shown as done 0 to 12. |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"current":2,"items":["Welcome","Product demo","Roadmap","Customer story","Q&A"],"title":"Today"},"template":"agenda"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"agenda","params":{"current":3}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
