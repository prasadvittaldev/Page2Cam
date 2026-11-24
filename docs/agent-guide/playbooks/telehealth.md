# Doctors and telehealth

A calm, trustworthy picture for video consultations: the clinician clearly visible, the room private, the practice's name and nothing else on screen.

## Background

- `set_background {"full": "blurred-room"}`: the room stays recognisable as a clinic but nothing in it is readable (charts, screens, other patients' files).
- Or a plain colour (`{"full": "colour", "full_colour": "#e8eef5"}`) for a neutral look.
- Never `screen` while a patient record could be open on that monitor.

## Webcam

- `set_overlay {"style": "cutout_full", "x": 0.5}`: the clinician centred, full height. With a plain colour background, `refined_edges` true gives cleaner hair edges.

## Graphics

```
add_layer {"kind": "template", "template": "lower-third", "params": {"title": "Dr. Maya Lee", "subtitle": "General practitioner, Riverside Clinic", "theme": "minimal"}, "hide_after_s": 20}
add_layer {"kind": "template", "template": "name-badge", "params": {"name": "Dr. Maya Lee", "line": "GP"}, "visible": false}
add_layer {"kind": "template", "template": "logo-bug", "params": {"image": "https://example.com/clinic-logo.png"}, "visible": false}
add_layer {"kind": "template", "template": "checklist", "params": {"title": "Before we finish", "items": [["Questions answered", "no"], ["Next steps agreed", "no"], ["Follow-up booked", "no"]]}, "visible": false}
```

Use the clinician's name and role from the user; the practice logo only if they give one. The checklist holds generic steps only.

## Steps

1. `status`, `start` if stopped; background and webcam as above; `snapshot`.
2. Before the patient joins: lower third (it hides itself after 20 s) so the patient sees who they are talking to.
3. During the consultation: nothing on screen, or only the small name badge in a corner away from the face.
4. Closing: show the checklist and tick items live (`update_layer {"id": "checklist", "params": {"items": [["Questions answered", "yes"], ["Next steps agreed", "yes"], ["Follow-up booked", "no"]]}}`).
5. End: `clear_layers`. Save the look once as a scene (`save_scene {"name": "Consultation", "include_background": true, "include_webcam": true}`) and load it before each session.

## Rules

- Never show a patient's name, date of birth, diagnosis, medication, results or any case detail in a graphic, even if the user mentions them.
- Prefer the blurred-room background; never share a screen with records on it.
- No tickers, countdowns or moving graphics while the patient talks.
- Check every change with `snapshot`; nothing may cover the clinician's face.
- Remind the user that their platform's consent and recording rules still apply; Page2Cam does not record.
