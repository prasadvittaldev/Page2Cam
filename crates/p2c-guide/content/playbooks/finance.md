---
title: Financial advisers and analysts
group: business
aliases: financial adviser, financial advisor, analyst, wealth manager, accountant, investment, banker, stocks, market update, trader, financial planner, investor relations
prompt: Set up for a client review or market update
args: name, title, company
---
# Financial advisers and analysts

Two different jobs with one rule: client meetings stay private and calm; public market updates are clear, sourced and always carry a disclaimer.

## Background

- Client meetings: `set_background {"full": "blurred-room"}`: nothing on the desk or wall is readable. Never `screen` while a client portfolio, CRM or banking app could be open on that monitor (it shares the whole monitor).
- Public streams and recorded market updates: a calm colour (`{"full": "colour", "full_colour": "#0d1b2a"}`) or a public chart page with `open_url`.

## Webcam

- Client meetings: `set_overlay {"style": "cutout_full", "x": 0.5}`, centred.
- Market updates: `{"style": "cutout_full", "x": 0.3}`, charts on the right.

## Graphics

```
add_layer {"kind": "template", "template": "disclaimer-bar", "params": {"text": "For information only. Not investment advice. Past performance is no guide to future returns.", "label": "Disclaimer"}}
add_layer {"kind": "template", "template": "lower-third", "params": {"title": "Daniel Osei", "subtitle": "Chartered Financial Planner, Example Wealth"}, "hide_after_s": 15}
add_layer {"kind": "template", "template": "line-chart", "params": {"title": "S&P 500, last 5 days (source: exchange data)", "points": [["Mon", "5980"], ["Tue", "6012"], ["Wed", "5995"], ["Thu", "6044"], ["Fri", "6071"]]}, "visible": false}
add_layer {"kind": "template", "template": "kpi-cards", "params": {"title": "Markets at close (source: exchange data)", "kpis": [["S&P 500", "6,071", "+0.4%", "Fri"], ["10y yield", "4.12%", "-0.03", "Fri"], ["EUR/USD", "1.084", "+0.2%", "Fri"]]}, "visible": false}
add_layer {"kind": "template", "template": "agenda", "params": {"title": "This week", "items": ["Markets in brief", "Rates outlook", "What it means for savers", "Your questions"], "current": 1}, "visible": false}
add_layer {"kind": "template", "template": "confidential-badge", "params": {"text": "CONFIDENTIAL", "note": "Client meeting, do not record"}, "visible": false}
```

## Steps

1. `status`, `start` if stopped; background and webcam for the job; `snapshot`.
2. Public stream or recording: the disclaimer-bar goes on FIRST and stays visible the whole time, start to end; check it with `snapshot` before `start_stream`. Save as `save_scene {"name": "market-update", "include_background": true, "include_webcam": true}` so it comes back with the look.
3. Intro: lower third, then the agenda.
4. Market data: one chart or kpi-cards at a time, with the source and the date in the title. Update with the latest public figures only: `update_layer {"id": "kpi-cards", "params": {"kpis": [["S&P 500", "6,090", "+0.3%", "Mon"], ["10y yield", "4.10%", "-0.02", "Mon"], ["EUR/USD", "1.086", "+0.2%", "Mon"]]}}`. Move the agenda: `update_layer {"id": "agenda", "params": {"current": 2}}`.
5. Q&A: question-card for public questions; never answer personal advice questions on a public stream.
6. Client meetings: blurred room, the lower third once, the confidential badge if the user wants it; no figures on screen at all. Save as `"client"`.
7. End: `clear_layers` (the disclaimer goes last, after the stream has stopped).

## Rules

- Never show account numbers, balances, client holdings, client names, tax details or anything from a client file, even if the user mentions them.
- Prefer the blurred-room background for client meetings; never share a screen with client data on it.
- While streaming or recording publicly the `disclaimer-bar` stays visible the whole time; do not hide it for cards or breaks.
- Public market data only, always with its source and date; no predictions presented as facts, no buy or sell calls.
- Check every change with `snapshot`; nothing covers the face; 5% safe margin above the disclaimer bar.
- Clear graphics at the end.
