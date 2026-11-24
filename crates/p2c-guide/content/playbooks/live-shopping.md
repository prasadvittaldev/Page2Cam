---
title: Live shopping and product launches
group: business
aliases: live commerce, shopping stream, seller, retailer, ecommerce, e-commerce, product drop, product launch, shop, merch, flash sale
prompt: Set up my live shopping stream
args: product, company
---
# Live shopping and product launches

An energetic but honest selling show: the host with the product in hand, a product card with the price and offer, a stock bar and a countdown that are true, and a simple way to buy.

## Background

- `blurred-room` or the real room as a set (shelves, studio) so the product in hand stays the focus.
- Or the shop page with `open_url` while walking through sizes and details; `show_page` to return.
- `{"full": "colour", "full_colour": "#1b1b2f"}` with refined edges for a clean studio look.

## Webcam

- `set_overlay {"style": "cutout_full", "x": 0.35}`: host left, product card on the right (its default place).
- Close-up of the product on a second camera page: `box` at `{"x": 0.05, "y": 0.5, "width": 0.3}`.

## Graphics

```
add_layer {"kind": "template", "template": "product-card", "params": {"name": "Studio ANC headphones", "picture": "https://example.com/headphones.png", "price": "$79", "old_price": "$99", "badge": "-20%", "offer_duration": "15:00", "stock_left": 40, "stock_total": 40, "cta": "Link in the description"}, "visible": false}
add_layer {"kind": "template", "template": "live-badge", "params": {"label": "LIVE"}}
add_layer {"kind": "template", "template": "qr-card", "params": {"link": "https://example.com/shop/anc", "title": "Buy now", "caption": "example.com/shop/anc"}, "x": 0.74, "y": 0.5, "visible": false}
add_layer {"kind": "template", "template": "countdown", "params": {"label": "Launch in", "until": "19:00", "done_text": "On sale now"}, "visible": false}
add_layer {"kind": "template", "template": "comparison-table", "params": {"title": "Which one?", "headers": ["Model", "ANC", "ANC Pro"], "rows": [["Noise cancelling", "yes", "yes"], ["Battery", "30 h", "45 h"], ["Wireless charging", "no", "yes"], ["Price", "$79", "$129"]], "highlight": "a"}, "visible": false}
```

## Steps

1. `status`, `start` if stopped; background, webcam; `snapshot`.
2. Before the start: countdown to the launch time. `save_scene {"name": "intro", "include_background": true, "include_webcam": true}`.
3. Intro (2 minutes): live badge, host introduces the product; no prices yet.
4. Reveal: show product-card. Keep stock real: `update_layer {"id": "product-card", "params": {"stock_left": 12}}` from the user's shop numbers. Save as `"product"`.
5. Next product: `update_layer` the same card with its name, picture, price and stock (it does not reload), or comparison-table to compare two.
6. Viewer questions: question-card; answer sizes, shipping, returns.
7. Close: qr-card with the shop link, a thank-you card; `clear_layers`.

## Rules

- Prices, discounts, stock and offer end times must be true: never invent scarcity or a fake countdown. Ask the user for the real numbers.
- Show the old price only if the product really sold at it.
- One moving element at a time: the product card's countdown is it; no extra tickers.
- Keep the product and the host's face clear; check with `snapshot` after every change.
- 5% safe margin; clear graphics at the end.
