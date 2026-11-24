---
title: Teachers and tutors
group: care
aliases: teacher, tutor, tutoring, lecturer, professor, school, online class, lesson, educator, classroom, homework help, trainer
prompt: Set up for my online lesson
args: name, subject, topic
---
# Teachers and tutors

A clear classroom picture: the teacher beside the material, the lesson plan on screen, timers for tasks, and quizzes that make the class take part. Calm enough to concentrate.

## Background

- `open_url` with the lesson material (slides, a whiteboard app, a simulation); `show_page` to return after a generated background.
- `screen` for software lessons only on a monitor with nothing private on it: it shares the whole monitor, including grade books, email and notifications.
- Talking or reading aloud: `blurred-room` or a calm colour.

## Webcam

- `set_overlay {"style": "cutout_small", "x": 0.95}`: teacher bottom right, material readable.
- Explaining or story time: `{"style": "cutout_full", "x": 0.3}`, graphics on the right.

## Graphics

```
add_layer {"kind": "template", "template": "agenda", "params": {"title": "Today's lesson", "items": ["Warm-up", "Fractions recap", "New: decimals", "Practice", "Exit quiz"], "current": 1}}
add_layer {"kind": "template", "template": "countdown", "params": {"label": "Practice time", "duration": "10:00", "done_text": "Pens down"}, "visible": false}
add_layer {"kind": "template", "template": "poll-results", "params": {"question": "What is 0.5 as a fraction?", "options": [["1/5", "0"], ["1/2", "0"], ["5/10 only", "0"]], "show_votes": false}, "visible": false}
add_layer {"kind": "template", "template": "steps", "params": {"title": "How to convert", "steps": ["Write as tenths", "Simplify", "Check"], "current": 1}, "visible": false}
add_layer {"kind": "template", "template": "lower-third", "params": {"title": "Ms. Patel", "subtitle": "Year 6 Maths"}, "hide_after_s": 10, "visible": false}
add_layer {"kind": "template", "template": "be-right-back", "params": {"title": "Short break", "subtitle": "Stretch, drink some water", "duration": "5:00"}, "visible": false}
```

## Steps

1. `status`, `start` if stopped; lesson page, webcam; `snapshot`. Save as `save_scene {"name": "lesson", "include_background": true, "include_webcam": true}`; a talk-to-class look as `"explain"`.
2. Start (first 2 minutes): lower third in a new class, then the agenda.
3. Each part: `update_layer {"id": "agenda", "params": {"current": 3}}`, show it briefly, hide it so the material is readable.
4. Explaining a method: steps, advanced live (`update_layer {"id": "steps", "params": {"current": 2}}`).
5. Practice: show the countdown; hide everything else.
6. Quiz: poll-results with the class's answers from the user's quiz tool, totals only: `update_layer {"id": "poll-results", "params": {"options": [["1/5", "3"], ["1/2", "21"], ["5/10 only", "4"]]}}`.
7. Break: be-right-back. End: `clear_layers`.

## Rules

- Never show pupils' or students' full names, photos, grades, attendance, or anything about one child. Quiz results only as class totals.
- `screen` shares everything on that monitor; close grade books and email first.
- One moving element at a time (countdown or poll, not both); big, readable text.
- Nothing over the teacher's face; 5% safe margin; check with `snapshot`.
- Follow the school's rules on recording and on streaming lessons.
- Clear graphics at the end.
