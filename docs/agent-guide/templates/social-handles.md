# Social handles (`social-handles`)

Where to find you: one handle per line (or in a row), each with a short coloured platform label.

Category: Basics. Default place: x 0.03, y 0.6, w 0.27, h 0.3 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `handles` | table | `[]` | **required.** Rows of [platform, handle], e.g. [["YouTube", "@examplechannel"], ["LinkedIn", "Example Co"]]. Known: YouTube, X, Instagram, LinkedIn, GitHub, TikTok, Twitch, Facebook, Bluesky, Mastodon, Discord, Web, Email Rows of [platform, handle]. |
| `layout` | choice | `"column"` | column: one per line (a tall box); row: side by side (a wide, low box) Options: column, row. |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"handles":[["YouTube","@examplechannel"],["X","@example"],["LinkedIn","Example Co"],["GitHub","example/project"]]},"template":"social-handles"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"social-handles","params":{"handles":[["Instagram","@example.studio"],["TikTok","@example"],["Web","example.com"]]}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
