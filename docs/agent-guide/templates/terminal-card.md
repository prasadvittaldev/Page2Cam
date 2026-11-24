# Terminal (`terminal-card`)

A terminal window with a prompt, a command and its output; can type the command out and then reveal the output line by line.

Category: Software companies. Default place: x 0.5, y 0.08, w 0.45, h 0.5 (0..1 of the 1920x1080 output; give x/y/w/h to move or resize).

## Parameters

| name | type | default | notes |
|---|---|---|---|
| `command` | text | `""` | **required.** The command after the prompt |
| `prompt` | text | `"$"` | Prompt before the command, e.g. $ or ~/app $ |
| `output` | longtext | `""` | Output lines (about 12 fit; the last lines stay visible). Lines starting with error or warning are coloured |
| `title` | text | `""` | Window title, e.g. bash |
| `typing` | bool | `false` | Type the command out, then reveal the output line by line (restarts when the command or output changes) |
| `theme` | choice | `"brand"` | Look: brand (navy and blue), news (navy, white and red), minimal (white text, light shading) Options: brand, news, minimal. |
| `accent` | colour | `""` | Accent colour #rrggbb (empty: the look's own) |

## Add it

```
add_layer {"kind":"template","params":{"command":"cargo build --release","output":"   Compiling p2c-overlay v0.7.0\n   Compiling page2cam v0.7.0\n    Finished `release` profile [optimized] target(s) in 41.2s","prompt":"~/page2cam $","title":"bash","typing":true},"template":"terminal-card"}
```

## Change it live (no reload: timers and animations keep running)

```
update_layer {"id":"terminal-card","params":{"command":"cargo test -p p2c-overlay","output":"running 42 tests\n........................................\ntest result: ok. 42 passed; 0 failed"}}
```

Only the params you give change; `null` puts one back to its default. The layer id is the one add_layer returned (the template id unless taken).
