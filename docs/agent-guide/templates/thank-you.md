# Thank you (`thank-you`)

Full-frame closing card: a big thank-you, a line under it, optional social handles and an optional QR code for a link.

Category: Full-frame cards. Default place: covers the whole output (a full-frame card).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `title` | text | `"Thank you"` | Big heading |
| `subtitle` | text | `""` | Line under it, e.g. Slides and recording: example.com/talk (empty: none) |
| `handles` | table | `[]` | Optional rows of [platform, handle], e.g. [["YouTube", "@examplechannel"], ["Web", "example.com"]] Rows of [platform, handle]. |
| `link` | qr | `""` | Optional text or link shown as a QR code at the right (empty: no code) |
| `link_caption` | text | `""` | Short line under the QR code, e.g. the short link |
| `background` | colour | `""` | Card colour #rrggbb (empty: the look's) |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"handles":[["YouTube","@examplechannel"],["LinkedIn","Example Co"],["Web","example.com"]],"link":"https://example.com/talk","link_caption":"example.com/talk","subtitle":"Slides and recording on our site","title":"Thank you!"},"template":"thank-you"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"thank-you","params":{"subtitle":"See you next week","title":"Thanks for watching"}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
