# Be right back (`be-right-back`)

Full-frame break card on a calm, slowly drifting gradient: title, subtitle and an optional countdown.

Category: Full-frame cards. Default place: covers the whole output (a full-frame card).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `title` | text | `"Be right back"` | Big heading |
| `subtitle` | text | `""` | Line under it, e.g. Short break, back in five minutes (empty: none) |
| `duration` | duration | `0` | Count down this long from when it is shown or the value changes (seconds or m:ss; 0: no countdown) |
| `background` | colour | `""` | Card colour #rrggbb (empty: the look's) |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"duration":"5:00","subtitle":"Short break, we continue in a few minutes","title":"Be right back"},"template":"be-right-back"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"be-right-back","params":{"duration":"1:00","subtitle":"Almost there, grab your seat"}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
