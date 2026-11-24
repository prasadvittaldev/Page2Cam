# Question card (`question-card`)

An audience question for Q&A: a tag, the question, and who asked it and where (e.g. via chat).

Category: Lists and text. Default place: x 0.05, y 0.6, w 0.5, h 0.3 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `question` | longtext | `""` | **required.** The question as asked. Best under 200 characters; longer text gets smaller and is cut after 4 lines |
| `name` | text | `""` | Who asked (empty: anonymous, no name line) |
| `source` | text | `""` | Where it came from, e.g. via chat, via YouTube, from the room |
| `label` | text | `"Audience question"` | The small tag above the question |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"name":"Priya S.","question":"Will the new API support webhooks for every event, or only the billing ones?","source":"via chat"},"template":"question-card"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"question-card","params":{"name":"Marco D.","question":"Is there a free tier for open-source projects?","source":"via YouTube"}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
