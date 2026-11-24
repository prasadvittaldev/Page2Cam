# Keyboard shortcut (`keyboard-shortcut`)

Key caps joined by + (e.g. Ctrl + Shift + P) with an optional caption saying what the shortcut does.

Category: Software companies. Default place: x 0.05, y 0.76, w 0.36, h 0.14 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `keys` | list | `[]` | **required.** Up to 5 keys in order, e.g. ["Ctrl", "Shift", "P"]; symbols like ⌘ or ⇧ work too |
| `caption` | text | `""` | What it does, e.g. Open the command palette (empty: none) |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"caption":"Open the command palette","keys":["Ctrl","Shift","P"]},"template":"keyboard-shortcut"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"keyboard-shortcut","params":{"caption":"Search everywhere","keys":["Ctrl","K"]}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
