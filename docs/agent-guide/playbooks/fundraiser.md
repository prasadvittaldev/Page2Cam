# Nonprofits and fundraisers

A warm, honest appeal: the cause explained by real people, a goal bar that fills with true numbers, an easy way to give, and thanks to donors and sponsors without exposing anyone.

## Background

- `set_background {"full": "image"}` with a photo of the project (with consent of the people in it) or the organisation's brand image.
- A short film about the cause with `{"full": "youtube", "youtube": "<link>"}` between segments.
- Or `blurred-room` for an office or home appeal.

## Webcam

- `set_overlay {"style": "cutout_full", "x": 0.35}`: host left, goal and QR on the right.
- Interviews with two people on one camera: `box` at `{"x": 0.5, "width": 0.65}`.

## Graphics

```
add_layer {"kind": "template", "template": "goal-bar", "params": {"title": "Clean water for Kisumu", "current": 6200, "goal": 10000, "prefix": "$"}}
add_layer {"kind": "template", "template": "qr-card", "params": {"link": "https://example.org/donate", "title": "Donate now", "caption": "example.org/donate"}, "visible": false}
add_layer {"kind": "template", "template": "lower-third", "params": {"title": "Grace Mwangi", "subtitle": "Programme Lead, Example Water Trust"}, "hide_after_s": 15, "visible": false}
add_layer {"kind": "template", "template": "kpi-cards", "params": {"title": "Your gifts at work", "kpis": [["Wells built", "42", "", "since 2022"], ["People served", "18,500", "", ""], ["Cost per person", "$12", "", ""]]}, "visible": false}
add_layer {"kind": "template", "template": "sponsor-carousel", "params": {"heading": "Matched by", "sponsors": [["Northwind Cloud", ""], ["Riverside Bank", ""]], "seconds": 6}, "visible": false}
add_layer {"kind": "template", "template": "thank-you", "params": {"title": "Thank you!", "subtitle": "Every gift brings clean water closer", "link": "https://example.org/donate", "link_caption": "example.org/donate"}, "visible": false}
```

## Steps

1. `status`, `start` if stopped; background, webcam; `snapshot`. Save as `save_scene {"name": "appeal", "include_background": true, "include_webcam": true}`.
2. Opening (5 minutes): the host, the cause in one sentence, the goal bar.
3. Stories: guest lower thirds, kpi-cards with the charity's published figures, a short film. Hide graphics during emotional stories.
4. Asks (every 10-15 minutes): qr-card for a minute or two.
5. Live totals from the donation platform: `update_layer {"id": "goal-bar", "params": {"current": 7450}}`. Thank donors by first name only if they chose to be named; otherwise "an anonymous donor".
6. Match moments: sponsor-carousel while announcing a matched gift.
7. Close: final total on the goal bar, the thank-you card, then `clear_layers`.

## Rules

- Totals, impact figures and match offers must be true and up to date; never inflate or invent urgency.
- Never show donor amounts next to names without consent; never show donors' contact details.
- People in photos and films (especially children and beneficiaries) only with consent and dignity.
- One moving element at a time: the goal bar animates, so the sponsor carousel waits.
- Graphics off faces; 5% safe margin; check every change with `snapshot`.
- Clear graphics at the end.
