# Name badge (`name-badge`)

A compact name tag for a corner: the name and a short line such as pronouns or role.

Category: Basics. Default place: x 0.04, y 0.84, w 0.22, h 0.1 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `name` | text | `""` | **required.** The person's name, e.g. Alex Kim |
| `line` | text | `""` | Short second line, e.g. pronouns (she/her) or role (empty: name only) |
| `align` | choice | `"left"` | Which side of the box the tag sits on: left for a left corner, right for a right corner Options: left, right. |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"line":"she/her · Product","name":"Alex Kim"},"template":"name-badge"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"name-badge","params":{"line":"he/him · Engineering","name":"Sam Ortiz"}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
