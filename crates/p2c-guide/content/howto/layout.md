---
title: Layout: where things go on the picture
aliases: position, placement, safe area, geometry, coordinates, where
---
# Layout

The output is 1920x1080 (16:9). Every position and size is a fraction (0..1) of it: `x`, `y` = top-left corner, `w`, `h` = size. On a 16:9 picture a square box has `h = w * 16 / 9`.

- **Safe area:** keep text 5% from every edge (x and y between 0.05 and 0.95). Call apps crop and cover the edges (names, buttons, self view).
- **The person:** `get_overlay` says where the webcam is. With `cutout_full` the person stands at the bottom, full height, centred on `x`; with `cutout_small` about half the height; with `box` a rectangle at `x`, `y` of width `width`. Keep graphics beside or below the face, never over it.
- **Good spots:** lower third bottom left (x 0.05, y 0.74); logo or live badge top corners (y 0.05); timers top right; charts, agendas and QR cards on the side away from the person (for a person on the left, x ≥ 0.55); tickers and disclaimer bars along the bottom edge.
- **One moving thing at a time:** a running countdown or a ticker, not both next to an animated chart.
- **Mirror:** Teams and Zoom may show the user's own preview mirrored; the people in the call see it the right way round. Trust `snapshot`, not the self view.
- **Stacking:** later layers are in front; `reorder_layers` restacks (back to front).
- Full-frame cards cover the person: show them only between parts of the show.

Check every change with `snapshot` before moving on.
