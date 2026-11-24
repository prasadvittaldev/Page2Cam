# Poll results (`poll-results`)

Live poll results: the question and one bar per option with its percentage, the leader highlighted; new vote counts animate.

Category: Data. Default place: x 0.6, y 0.1, w 0.35, h 0.5 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `question` | text | `""` | **required.** The poll question |
| `options` | table | `[]` | **required.** 2 to 8 rows: option text, votes (a number) Rows of [option, votes]. |
| `show_votes` | bool | `true` | Show the total number of votes under the bars |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"options":[["Dark mode","412"],["Offline sync","268"],["API access","190"],["Plugins","95"]],"question":"Which feature should we build next?"},"template":"poll-results"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"poll-results","params":{"options":[["Dark mode","430"],["Offline sync","520"],["API access","214"],["Plugins","101"]]}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
