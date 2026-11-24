# Steps (`steps`)

Numbered steps in a row or a column: the current step is highlighted, earlier ones are ticked. Good for tutorials, onboarding and processes.

Category: Lists and text. Default place: x 0.15, y 0.78, w 0.7, h 0.14 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `steps` | list | `[]` | **required.** Up to 8 step names, in order, short (one to three words each) |
| `current` | number | `1` | The step in progress, 1-based (0: none). Earlier steps are shown ticked 0 to 8. |
| `layout` | choice | `"horizontal"` | horizontal: a row (wide, low box); vertical: a column (tall box, give it a taller rect) Options: horizontal, vertical. |
| `title` | text | `""` | Heading above the steps (empty: none) |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"current":2,"steps":["Sign up","Connect","Configure","Go live"]},"template":"steps"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"steps","params":{"current":3}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
