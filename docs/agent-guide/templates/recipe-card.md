# Recipe card (`recipe-card`)

A recipe panel for cooking shows: title, servings and time, the ingredients list and the step being done right now.

Category: Lists and text. Default place: x 0.62, y 0.08, w 0.33, h 0.72 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `title` | text | `""` | **required.** The dish, e.g. Lemon garlic pasta |
| `meta` | text | `""` | One line under the title, e.g. Serves 4 · 25 min |
| `ingredients` | list | `[]` | Up to 16 ingredients with amounts, e.g. 400 g spaghetti |
| `step` | longtext | `""` | The step being done now (empty: no step box). Keep it to one or two sentences |
| `step_label` | text | `"Now"` | Small tag above the step, e.g. Now or Step 3 |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"ingredients":["400 g spaghetti","3 cloves garlic","1 lemon, zest and juice","60 g butter","50 g parmesan","Fresh parsley"],"meta":"Serves 4 · 25 min","step":"Boil the spaghetti in well salted water for 9 minutes.","step_label":"Step 1","title":"Lemon garlic pasta"},"template":"recipe-card"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"recipe-card","params":{"step":"Melt the butter and gently fry the sliced garlic until golden.","step_label":"Step 2"}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
