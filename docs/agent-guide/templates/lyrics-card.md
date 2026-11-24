# Lyrics card (`lyrics-card`)

Centred lines of lyrics, a verse or scripture in the lower part of the picture, with an optional reference line (song title, John 3:16).

Category: Lists and text. Default place: x 0.1, y 0.64, w 0.8, h 0.28 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `lines` | longtext | `""` | **required.** The lines to show, one per line (newline-separated), at most 6 lines; keep each line short |
| `reference` | text | `""` | Small line under the text: a song title, a verse reference like John 3:16 (empty: none) |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"lines":"Amazing grace, how sweet the sound\nThat saved a wretch like me","reference":"Amazing Grace, verse 1"},"template":"lyrics-card"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"lyrics-card","params":{"lines":"I once was lost, but now am found\nWas blind, but now I see","reference":"Amazing Grace, verse 1"}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
