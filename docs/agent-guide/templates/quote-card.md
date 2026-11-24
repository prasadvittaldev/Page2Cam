# Quote card (`quote-card`)

A quotation with a large quotation mark, the author and their role; for testimonials, famous quotes and pull quotes.

Category: Lists and text. Default place: x 0.1, y 0.62, w 0.6, h 0.3 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `quote` | longtext | `""` | **required.** The quote itself, without quotation marks. Best under 200 characters; longer text gets smaller and is cut after 5 lines |
| `author` | text | `""` | Who said it (empty: no author line) |
| `role` | text | `""` | Their role, company or source, shown after the author |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"author":"Alan Kay","quote":"The best way to predict the future is to invent it.","role":"Computer scientist"},"template":"quote-card"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"quote-card","params":{"author":"Alan Kay","quote":"Simple things should be simple, complex things should be possible.","role":"Turing Award 2003"}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
