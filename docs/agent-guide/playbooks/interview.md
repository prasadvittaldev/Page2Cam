# Recruiters and job interviews

A friendly, neutral, professional picture, for either side of the table: the person clearly visible, a tidy background, their name and role once, and nothing that distracts or could seem unfair.

## Background

- `set_background {"full": "blurred-room"}`: hides a home or office without looking fake.
- Or a calm colour (`{"full": "colour", "full_colour": "#e9eef4"}`) or the company's employer-brand image for recruiters (`upload_background_image`, `{"full": "image"}`).
- A coding or case exercise: `open_url` with the exercise page; `screen` only on a monitor with nothing else open (it shares the whole monitor).

## Webcam

- `set_overlay {"style": "cutout_full", "x": 0.5}`: centred, eye level. Over a shared exercise: `{"style": "cutout_small", "x": 0.95}`.

## Graphics

```
add_layer {"kind": "template", "template": "lower-third", "params": {"title": "Jordan Lee", "subtitle": "Talent Partner, Example Co"}, "hide_after_s": 15}
add_layer {"kind": "template", "template": "name-badge", "params": {"name": "Jordan Lee", "line": "they/them · Talent"}, "visible": false}
add_layer {"kind": "template", "template": "agenda", "params": {"title": "Our 45 minutes", "items": ["Introductions", "Your experience", "Exercise", "Your questions", "Next steps"], "current": 1}, "visible": false}
add_layer {"kind": "template", "template": "steps", "params": {"title": "Hiring process", "steps": ["Screening", "Team interview", "Exercise", "Offer"], "current": 1}, "visible": false}
add_layer {"kind": "template", "template": "logo-bug", "params": {"image": "https://example.com/logo.png", "opacity": 0.7}, "visible": false}
```

For a job seeker use only the lower third or name badge with their own name and the role they apply for (if they want it).

## Steps

1. `status`, `start` if stopped; blurred room, cutout_full; `snapshot`. Save as `save_scene {"name": "interview", "include_background": true, "include_webcam": true}`.
2. First minute: lower third (hides itself), then the small name badge if wanted.
3. Recruiter: show the agenda at the start and at each part: `update_layer {"id": "agenda", "params": {"current": 3}}`; hide it in between.
4. Exercise: `open_url` with the task, cutout_small; hide all other graphics.
5. Candidate questions: graphics off.
6. Next steps (last 5 minutes): the hiring process steps with the candidate's current stage, then `clear_layers`.

## Rules

- Never show candidate data on screen: CV details, salary, scores, notes, other candidates' names.
- Same graphics for every candidate: no difference in look or treatment.
- No timers on the candidate; no tickers or moving graphics.
- Keep faces clear and text in the 5% safe margin; check with `snapshot`.
- Remind the user that recording an interview needs the candidate's consent.
- Clear graphics at the end.
