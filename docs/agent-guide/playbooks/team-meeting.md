# Team leads and internal meetings

A tidy, low-key picture for everyday meetings: the lead clearly visible, the agenda and timebox on screen when it helps, and nothing flashy. The goal is a shorter, more focused meeting.

## Background

- `set_background {"full": "blurred-room"}` for a home office; or the team board / doc with `open_url` when walking through it.
- Internal dashboards with `screen` only on a monitor with nothing private (it shares the whole monitor).

## Webcam

- `set_overlay {"style": "cutout_small", "x": 0.05}` over a shared page; agenda on the right.
- Just talking: `{"style": "box", "x": 0.5, "y": 0.5, "width": 0.7}` or cutout_full at `"x": 0.35`.

## Graphics

```
add_layer {"kind": "template", "template": "agenda", "params": {"title": "Weekly sync", "items": ["Wins", "Blockers", "Priorities", "Decisions"], "current": 1}}
add_layer {"kind": "template", "template": "speaker-timer", "params": {"speaker": "Updates: Maya", "duration": "2:00", "warn_at": 30}, "visible": false}
add_layer {"kind": "template", "template": "checklist", "params": {"title": "Action items", "items": [["Ship onboarding fix", "no"], ["Hire plan to finance", "no"], ["Book offsite venue", "no"]]}, "visible": false}
add_layer {"kind": "template", "template": "progress-donut", "params": {"value": 68, "max": 100, "label": "Q4 goals"}, "visible": false}
add_layer {"kind": "template", "template": "confidential-badge", "params": {"text": "INTERNAL", "note": "Do not share outside the team", "colour": "amber"}, "visible": false}
```

## Steps

1. `status`, `start` if stopped; background and webcam; `snapshot`.
2. Start (first minute): the agenda. Save the look: `save_scene {"name": "weekly", "include_background": true, "include_webcam": true}`, then `load_scene` it every week.
3. Round of updates: speaker-timer per person: `update_layer {"id": "speaker-timer", "params": {"speaker": "Updates: Jonas"}}` (restarts it). Hide the agenda meanwhile.
4. Move on: `update_layer {"id": "agenda", "params": {"current": 2}}`.
5. Goals: progress-donut with the real figure for a minute.
6. End (last 5 minutes): checklist of action items agreed in the meeting; tick done ones in the next meeting: `update_layer {"id": "checklist", "params": {"items": [["Ship onboarding fix", "yes"], ["Hire plan to finance", "no"], ["Book offsite venue", "yes"]]}}`. Then `clear_layers`.

## Rules

- Never put performance feedback, salaries, HR matters or personal details about a team member on screen.
- Show the INTERNAL badge when the user says the content is internal or confidential.
- Keep it light: one graphic at a time, no tickers or countdowns besides the speaker timer.
- Nothing over faces; 5% safe margin; check with `snapshot`.
- Clear graphics at the end.
