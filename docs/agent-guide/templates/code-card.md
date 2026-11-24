# Code card (`code-card`)

A syntax-highlighted code snippet in an editor-style card, optionally typed out.

Category: Software companies. Default place: x 0.5, y 0.08, w 0.45, h 0.55 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `code` | longtext | `""` | **required.** The code (keep it short: about 15 lines fit) |
| `language` | choice | `"js"` | Language for highlighting Options: js, ts, py, rust, go, java, cs, sql, sh, json, text. |
| `filename` | text | `""` | Title bar text, e.g. main.rs |
| `typing` | bool | `false` | Type the code out character by character |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"code":"fn main() {\n    let name = \"Page2Cam\";\n    println!(\"Hello from {name}!\");\n}","filename":"main.rs","language":"rust"},"template":"code-card"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"code-card","params":{"code":"fn main() {\n    let name = \"Page2Cam 2.0\";\n    println!(\"Hello from {name}!\");\n}\n// shipped"}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
