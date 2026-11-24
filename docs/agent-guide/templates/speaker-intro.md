# Speaker intro (`speaker-intro`)

Full-frame speaker introduction: photo (or initials), name, role, company and a short bio.

Category: Full-frame cards. Default place: covers the whole output (a full-frame card).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `name` | text | `""` | **required.** The speaker's name |
| `role` | text | `""` | Title or role, e.g. Principal Engineer |
| `company` | text | `""` | Company or organisation, shown small above the name (empty: none) |
| `bio` | longtext | `""` | A short bio, 1 to 3 sentences; about 300 characters are shown |
| `photo` | asset | `""` | Portrait: an https address or a data:image URI (empty: the initials in a circle) |
| `background` | colour | `""` | Card colour #rrggbb (empty: the look's) |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"bio":"Maya leads the real-time video team at Example Labs. She has spent ten years building streaming systems and loves making complex tech feel simple.","company":"Example Labs","name":"Dr. Maya Chen","role":"Principal Engineer"},"template":"speaker-intro"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"speaker-intro","params":{"bio":"Jordan shapes how millions of people use Example products every day.","name":"Jordan Patel","role":"Head of Design"}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
