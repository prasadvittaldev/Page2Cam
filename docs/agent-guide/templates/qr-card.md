# QR card (`qr-card`)

A QR code with a heading and a short line: a link, a sign-up, a download.

Category: Call to action. Default place: x 0.74, y 0.5, w 0.21, h 0.42 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `link` | qr | `""` | **required.** The text or link the code opens |
| `title` | text | `"Scan me"` | Heading above the code |
| `caption` | text | `""` | Line under the code, e.g. the short link |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"caption":"example.com/signup","link":"https://example.com/signup","title":"Get the slides"},"template":"qr-card"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"qr-card","params":{"caption":"example.com/beta","link":"https://example.com/beta","title":"Join the beta"}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
