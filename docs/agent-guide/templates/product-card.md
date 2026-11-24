# Product card (`product-card`)

Live-shopping card: product picture, name, price with the old price struck through, a discount badge, an optional offer countdown and a stock bar.

Category: Call to action. Default place: x 0.69, y 0.1, w 0.26, h 0.8 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `name` | text | `""` | **required.** Product name |
| `picture` | asset | `""` | Product picture: an https address or a data:image URI (empty: no picture) |
| `price` | text | `""` | **required.** Price as shown, e.g. $79 or 79,00 EUR |
| `old_price` | text | `""` | Earlier price, shown struck through (empty: none) |
| `badge` | text | `""` | Short badge, e.g. -20% or NEW (empty: none) |
| `offer_until` | datetime | `""` | Offer ends at this time (18:30 or 2026-12-31T18:30); wins over offer_duration |
| `offer_duration` | duration | `0` | Or the offer runs this long from when it is shown (seconds or m:ss; 0 and no time: no countdown) |
| `offer_text` | text | `"Offer ends in"` | Text before the countdown |
| `stock_left` | number | `0` | Items left (shown when stock_total is above 0) 0 to 1000000. |
| `stock_total` | number | `0` | Items at the start, for the stock bar (0: no stock bar) 0 to 1000000. |
| `cta` | text | `""` | Call to action at the bottom, e.g. Link in the description (empty: none) |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"badge":"-20%","cta":"Link in the description","name":"Studio ANC headphones","offer_duration":"15:00","old_price":"$99","picture":"data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHZpZXdCb3g9IjAgMCA0MDAgMzAwIj48ZGVmcz48bGluZWFyR3JhZGllbnQgaWQ9ImciIHgxPSIwIiB5MT0iMCIgeDI9IjEiIHkyPSIxIj48c3RvcCBvZmZzZXQ9IjAiIHN0b3AtY29sb3I9IiNmNGY3ZmIiLz48c3RvcCBvZmZzZXQ9IjEiIHN0b3AtY29sb3I9IiNkM2RjZTciLz48L2xpbmVhckdyYWRpZW50PjxsaW5lYXJHcmFkaWVudCBpZD0iYyIgeDE9IjAiIHkxPSIwIiB4Mj0iMCIgeTI9IjEiPjxzdG9wIG9mZnNldD0iMCIgc3RvcC1jb2xvcj0iIzJmMzk0YiIvPjxzdG9wIG9mZnNldD0iMSIgc3RvcC1jb2xvcj0iIzEwMTQxYyIvPjwvbGluZWFyR3JhZGllbnQ+PC9kZWZzPjxyZWN0IHdpZHRoPSI0MDAiIGhlaWdodD0iMzAwIiBmaWxsPSJ1cmwoI2cpIi8+PGVsbGlwc2UgY3g9IjIwMCIgY3k9IjI2NCIgcng9IjEyNSIgcnk9IjEyIiBmaWxsPSIjMDAwIiBvcGFjaXR5PSIuMTIiLz48cGF0aCBkPSJNMTEyIDE3MiBDMTEyIDkyIDE1MiA1NiAyMDAgNTYgQzI0OCA1NiAyODggOTIgMjg4IDE3MiIgZmlsbD0ibm9uZSIgc3Ryb2tlPSJ1cmwoI2MpIiBzdHJva2Utd2lkdGg9IjE2IiBzdHJva2UtbGluZWNhcD0icm91bmQiLz48cmVjdCB4PSI4NCIgeT0iMTUwIiB3aWR0aD0iNTgiIGhlaWdodD0iMTA0IiByeD0iMjciIGZpbGw9InVybCgjYykiLz48cmVjdCB4PSIyNTgiIHk9IjE1MCIgd2lkdGg9IjU4IiBoZWlnaHQ9IjEwNCIgcng9IjI3IiBmaWxsPSJ1cmwoI2MpIi8+PHJlY3QgeD0iMTMwIiB5PSIxNzAiIHdpZHRoPSIxNCIgaGVpZ2h0PSI2NCIgcng9IjciIGZpbGw9IiMwYjYzZjAiLz48cmVjdCB4PSIyNTYiIHk9IjE3MCIgd2lkdGg9IjE0IiBoZWlnaHQ9IjY0IiByeD0iNyIgZmlsbD0iIzBiNjNmMCIvPjwvc3ZnPg==","price":"$79","stock_left":7,"stock_total":40},"template":"product-card"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"product-card","params":{"badge":"-30%","price":"$69","stock_left":3}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
