---
title: Business presenters and sales demos
group: business
aliases: sales, salesperson, account executive, sales call, pitch, presenter, business presentation, prospect, sales rep, discovery call, proposal
prompt: Set up for a sales call
args: name, company, product
---
# Business presenters and sales demos

A professional, focused picture for a pitch or a customer demo: the presenter beside the product, a short agenda, the key numbers when they matter, and a clear next step at the end. Nothing on screen distracts from the conversation.

## Background

- `open_url` with the product, the deck or the customer's own website: the presenter in front of what is being discussed. `show_page` returns to it after a generated background.
- `screen` for a desktop app demo: it shares the whole monitor, so close email, chat and other customers' data first and turn on Do Not Disturb.
- Before the demo starts: `{"full": "colour", "full_colour": "#0d1b2a"}` or a branded image.

## Webcam

- `set_overlay {"style": "cutout_small", "x": 0.95}`: presenter bottom right, the product readable on the left.
- Relationship moments (intro, pricing, close): `{"style": "cutout_full", "x": 0.3}` over the colour background.

## Graphics

```
add_layer {"kind": "template", "template": "lower-third", "params": {"title": "Alex Kim", "subtitle": "Account Executive, Example Co"}, "hide_after_s": 15}
add_layer {"kind": "template", "template": "agenda", "params": {"title": "Today", "items": ["Your goals", "Live demo", "Pricing", "Next steps"], "current": 1}, "visible": false}
add_layer {"kind": "template", "template": "comparison-table", "params": {"title": "Plans", "headers": ["Feature", "Starter", "Business"], "rows": [["Users", "5", "Unlimited"], ["SSO", "no", "yes"], ["Support", "Email", "24/7 phone"], ["Price per user", "$12", "$29"]], "highlight": "b"}, "visible": false}
add_layer {"kind": "template", "template": "kpi-cards", "params": {"kpis": [["Time saved", "6 h/week", "", "per rep"], ["Onboarding", "2 days", "-60%", "vs before"], ["ROI", "4.2x", "", "first year"]]}, "visible": false}
add_layer {"kind": "template", "template": "qr-card", "params": {"link": "https://example.com/book", "title": "Book a follow-up", "caption": "example.com/book"}, "visible": false}
add_layer {"kind": "template", "template": "logo-bug", "params": {"image": "https://example.com/logo.png", "opacity": 0.8}, "visible": false}
```

Numbers on kpi-cards must be the user's real, approved figures; leave the layer out otherwise.

## Steps

1. `status`, `start` if stopped; colour background, cutout_full; `snapshot`. Save as `save_scene {"name": "intro", "include_background": true, "include_webcam": true}`.
2. Intro (first 2-3 minutes): lower third, then the agenda.
3. Demo: `open_url` (or `screen`), cutout_small; `update_layer {"id": "agenda", "params": {"current": 2}}`, then hide the agenda so the product is readable. Save as `"demo"`; switch between `intro` and `demo` with `load_scene`.
4. Value and pricing: kpi-cards or comparison-table, one at a time; `update_layer {"id": "comparison-table", "params": {"highlight": "a"}}` if the customer leans to the smaller plan.
5. Questions: graphics off, cutout_full; listen.
6. Close: agenda on "Next steps" (`current` 4), qr-card for booking a follow-up. End with `clear_layers`.

## Rules

- Never show another customer's name, data or pricing; check the shared page and `snapshot` before showing.
- No figures that the user has not confirmed; no claims about competitors that cannot be backed up.
- One moving element at a time; nothing over the presenter's face.
- 5% safe margin; Teams and Zoom crop the edges.
- Clear graphics at the end.
