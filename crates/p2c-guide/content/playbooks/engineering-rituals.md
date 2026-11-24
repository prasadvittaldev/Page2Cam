---
title: Sprint reviews, all-hands and incident updates
group: software
aliases: sprint review, demo day, all-hands, all hands, town hall, incident update, incident, post-mortem, postmortem, retro, retrospective, engineering manager, outage update, sprint demo
prompt: Set up our sprint review or all-hands
args: company, topic, name
---
# Sprint reviews, all-hands and incident updates

Internal engineering moments with three looks: a sprint review or demo day (what shipped, who built it), an all-hands (metrics, roadmap, questions), and an incident update or post-mortem (calm, factual, current status first).

## Background

- Demos: the feature on staging with `open_url`, or `screen` on a monitor with nothing private (it shares the whole monitor; no customer data, no secrets in terminals).
- All-hands and incidents: `{"full": "colour", "full_colour": "#0d1b2a"}` or `blurred-room`.

## Webcam

- Demos: `set_overlay {"style": "cutout_small", "x": 0.95}`. Talking: `{"style": "cutout_full", "x": 0.3}`, graphics on the right.

## Graphics

```
add_layer {"kind": "template", "template": "agenda", "params": {"title": "Sprint 42 review", "items": ["Goal and outcome", "Demos", "Metrics", "Next sprint"], "current": 1}}
add_layer {"kind": "template", "template": "progress-donut", "params": {"value": 34, "max": 41, "label": "Points done", "centre": "34/41"}, "visible": false}
add_layer {"kind": "template", "template": "kpi-cards", "params": {"title": "Since last all-hands", "kpis": [["Deploys", "212", "+18%", "per month"], ["Uptime", "99.97%", "+0.02%", "30 days"], ["p95 latency", "182 ms", "-9%", "API"], ["Open bugs", "64", "-12%", "P1-P3"]], "invert": "p95 latency, Open bugs"}, "visible": false}
add_layer {"kind": "template", "template": "status-card", "params": {"service": "Checkout API", "status": "partial-outage", "incident": "Elevated errors on EU checkout", "updates": [["13:48", "Investigating increased errors on EU checkout."]], "components": [["API", "operational"], ["Checkout (EU)", "partial-outage"], ["Webhooks", "operational"]]}, "visible": false}
add_layer {"kind": "template", "template": "architecture-diagram", "params": {"title": "What failed", "nodes": [["gw", "API gateway", "1"], ["orders", "Orders service", "2"], ["cache", "Cache cluster", "3"], ["db", "Postgres", "3"]], "edges": [["gw", "orders", ""], ["orders", "cache", "reads"], ["orders", "db", "SQL"]], "highlight": "cache"}, "visible": false}
add_layer {"kind": "template", "template": "confidential-badge", "params": {"text": "INTERNAL", "note": "Do not share outside the company", "colour": "amber"}, "visible": false}
```

## Steps

Sprint review / demo day:
1. `status`, `start` if stopped; `snapshot`. Agenda, then the progress donut with the real sprint numbers.
2. Each demo: `update_layer {"id": "agenda", "params": {"current": 2}}`; a lower third per presenter (name and team); hide the agenda during the demo. `save_scene {"name": "sprint-review", "include_background": true, "include_webcam": true}`.

All-hands:
3. INTERNAL badge if the user says it is internal; kpi-cards with approved figures, a roadmap-timeline, then question-card for submitted questions (no asker names unless they agreed). Save as `"all-hands"`.

Incident update:
4. status-card first, current status at the top. Update it live as the incident moves, newest first: `update_layer {"id": "status-card", "params": {"status": "degraded", "updates": [["14:32", "Fix deployed, monitoring recovery."], ["14:05", "Cause identified: a faulty cache node."], ["13:48", "Investigating increased errors on EU checkout."]]}}`. Resolved: `"status": "operational"` and a final update. Save as `"incident"`.

Post-mortem:
5. Timeline in the status-card, the failing part in architecture-diagram (`highlight` it), follow-ups as a checklist ticked in later reviews.
6. End of every ritual: `clear_layers`.

## Rules

- Post-mortems are blameless: no individual blamed on screen, no names next to mistakes.
- Never show customer names or data, logs with personal data, secrets, tokens, internal hostnames or IPs; redact before sharing a terminal or dashboard.
- Incident updates: facts only, times in one time zone, no guesses about cause until confirmed.
- One moving element at a time; nothing over faces; 5% safe margin; check every change with `snapshot`.
- Clear graphics at the end.
