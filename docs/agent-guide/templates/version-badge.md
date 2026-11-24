# Version badge (`version-badge`)

A badge announcing a release: a small "Now shipping" kicker, the version in large type and an optional tagline.

Category: Software companies. Default place: x 0.7, y 0.06, w 0.25, h 0.2 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `version` | text | `""` | **required.** Version, e.g. v2.0 (short) |
| `kicker` | text | `"Now shipping"` | Small line above the version (empty: none) |
| `tagline` | text | `""` | Line under the version, e.g. the headline feature (empty: none) |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"tagline":"Overlay templates for every show","version":"v2.0"},"template":"version-badge"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"version-badge","params":{"tagline":"Now with scenes and playbooks","version":"v2.1"}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
