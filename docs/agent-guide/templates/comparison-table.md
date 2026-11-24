# Comparison table (`comparison-table`)

Compares two or three options feature by feature; yes/no cells become check marks and crosses, one column can be highlighted.

Category: Data. Default place: x 0.5, y 0.1, w 0.45, h 0.55 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `title` | text | `""` | Heading (empty: none) |
| `headers` | list | `[]` | Header row: first the feature column's heading, then one name per option, e.g. ["Feature", "Free", "Pro"] (empty: no header row) |
| `rows` | table | `[]` | **required.** Up to 10 rows: feature, then a value per option (b and c optional). "yes" shows a check mark, "no" a cross, anything else as text Rows of [feature, a, b, c]. |
| `highlight` | choice | `"none"` | Option column to highlight (a, b or c), e.g. the recommended plan Options: none, a, b, c. |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"headers":["Feature","Free","Pro","Team"],"highlight":"b","rows":[["Virtual camera","yes","yes","yes"],["Live streaming","no","yes","yes"],["Overlay layers","3","Unlimited","Unlimited"],["Agent control","no","yes","yes"],["Shared templates","no","no","yes"],["Price per month","$0","$12","$29"]],"title":"Plans compared"},"template":"comparison-table"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"comparison-table","params":{"highlight":"c","title":"Best for teams"}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
