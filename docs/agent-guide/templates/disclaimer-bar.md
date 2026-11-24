# Disclaimer bar (`disclaimer-bar`)

A thin full-width bar along the bottom edge with one line of small print, e.g. a disclaimer or notice.

Category: Basics. Default place: x 0, y 0.952, w 1, h 0.048 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `text` | text | `""` | **required.** The small print, one line, e.g. Not financial advice. For information only |
| `label` | text | `""` | Short tag at the left, e.g. NOTICE or DISCLAIMER (empty: none) |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"label":"Disclaimer","text":"Not financial advice. For information only; do your own research."},"template":"disclaimer-bar"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"disclaimer-bar","params":{"text":"Opinions are the speaker's own and not those of Example Co."}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
