---
title: Real estate agents
group: business
aliases: realtor, estate agent, property, house tour, listing, broker, letting agent, open house, property tour, home seller, virtual viewing
prompt: Set up my property tour
args: name, company, topic
---
# Real estate agents

A clear, inviting picture for virtual viewings and listing presentations: the property large on screen, the agent small beside it, the key facts and price at a glance, and an easy way to book a visit.

## Background

- `set_background {"full": "image"}` with a listing photo (`upload_background_image`), changed room by room; or the listing page / 3D tour with `open_url`.
- Walking through a house with a phone: the phone's video as the page if available, the agent in a small box.
- A plan or map on a monitor: `screen`, which shares the whole monitor; close email and client files first.

## Webcam

- `set_overlay {"style": "cutout_small", "x": 0.05}`: agent bottom left, facts on the right.
- Introductions and market talk: `{"style": "cutout_full", "x": 0.3}` over the listing photo.

## Graphics

```
add_layer {"kind": "template", "template": "lower-third", "params": {"title": "Sophie Martin", "subtitle": "Senior Agent, Example Homes"}, "hide_after_s": 15}
add_layer {"kind": "template", "template": "kpi-cards", "params": {"title": "12 Elm Street", "kpis": [["Price", "$645,000", "", ""], ["Bedrooms", "4", "", ""], ["Living area", "182 m²", "", ""], ["Garden", "South", "", ""]]}, "visible": false}
add_layer {"kind": "template", "template": "checklist", "params": {"title": "Highlights", "items": [["New kitchen 2025", "yes"], ["Solar panels", "yes"], ["Garage", "yes"], ["Near schools", "yes"]], "show_count": false}, "visible": false}
add_layer {"kind": "template", "template": "qr-card", "params": {"link": "https://example.com/12-elm/viewing", "title": "Book a viewing", "caption": "example.com/12-elm"}, "visible": false}
add_layer {"kind": "template", "template": "logo-bug", "params": {"image": "https://example.com/agency-logo.png", "opacity": 0.85}, "visible": false}
```

## Steps

1. `status`, `start` if stopped; first listing photo, cutout_full; `snapshot`.
2. Intro (1 minute): lower third and the agency logo.
3. Key facts: kpi-cards for 20-30 seconds, then hide. Save as `save_scene {"name": "listing", "include_background": true, "include_webcam": true}`.
4. Room by room: new photo with `upload_background_image`, cutout_small; one sentence per room. Show highlights near the end.
5. Price change or open-house news: `update_layer {"id": "kpi-cards", "params": {"kpis": [["Price", "$629,000", "-2.5%", "reduced"], ["Bedrooms", "4", "", ""], ["Living area", "182 m²", "", ""], ["Garden", "South", "", ""]]}}`.
6. Questions and close: qr-card to book a viewing for 1-2 minutes, then `clear_layers`.

## Rules

- Never show the seller's name, the exact address of an occupied home unless the user confirms it is public, people in the photos, or private documents.
- Facts and prices exactly as in the official listing.
- One graphic at a time, away from the agent's face; check with `snapshot` after every change.
- 5% safe margin; clear graphics at the end.
