# Now playing (`now-playing`)

The song that is playing: title, artist, optional cover picture and an optional progress bar.

Category: Lists and text. Default place: x 0.05, y 0.8, w 0.32, h 0.12 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `title` | text | `""` | **required.** Song title |
| `artist` | text | `""` | Artist or band (empty: none) |
| `cover` | asset | `""` | Cover art: an https address or a data:image URI (empty: a music-note tile) |
| `progress` | number | `-1` | How far into the song, 0 to 100 percent (-1: no progress bar) -1 to 100. |
| `label` | text | `"Now playing"` | Small tag above the title |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"artist":"M83","progress":35,"title":"Midnight City"},"template":"now-playing"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"now-playing","params":{"artist":"M83","progress":70,"title":"Wait"}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
