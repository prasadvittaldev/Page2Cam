# Caption bar (`caption-bar`)

Manual captions: up to two lines of large white text on a dark band, centred near the bottom, readable on any background.

Category: Basics. Default place: x 0.1, y 0.77, w 0.8, h 0.17 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `text` | longtext | `""` | **required.** The caption. At most two lines show (about 90 characters); replace it with update_layer as the talk moves on |
| `speaker` | text | `""` | Who is speaking, shown as a small tag above (empty: none) |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"speaker":"Alex","text":"Welcome everyone, and thanks for joining today's session on the new release."},"template":"caption-bar"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"caption-bar","params":{"speaker":"Sam","text":"First, let's look at what changed since the last version."}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
