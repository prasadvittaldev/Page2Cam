# Section title (`section-title`)

Full-frame chapter heading: a big title with an optional number such as 01 and a small kicker line above it.

Category: Full-frame cards. Default place: covers the whole output (a full-frame card).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `title` | text | `""` | **required.** The chapter heading, e.g. Results from the pilot |
| `number` | text | `""` | Optional chapter number, e.g. 01 or 2 (empty: none) |
| `kicker` | text | `""` | Small line above the title, e.g. Part two (empty: none) |
| `subtitle` | text | `""` | Optional line under the title |
| `background` | colour | `""` | Card colour #rrggbb (empty: the look's) |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"kicker":"Part two","number":"02","title":"Results from the pilot"},"template":"section-title"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"section-title","params":{"kicker":"Part three","number":"03","title":"What comes next"}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
