---
title: Fitness and yoga instructors
group: creators
aliases: yoga, personal trainer, workout, pilates, gym, hiit, fitness coach, instructor, exercise class, stretching, mobility
prompt: Set up my workout class
args: name, topic
---
# Fitness and yoga instructors

A clear class picture: the instructor's whole body visible, a big timer for the current interval, and the next exercise on screen so people can follow without stopping.

## Background

- `set_background {"full": "colour", "full_colour": "#10202c"}` or a calm image: a cut-out instructor on a plain colour is easiest to follow.
- `blurred-room` keeps a real studio feel without clutter.
- A music playlist page only with page sound for the stream (`set_audio {"page_on": true}`), never copyrighted music on a public stream without rights.

## Webcam

- `set_overlay {"style": "cutout_full", "x": 0.4}`: whole body, slightly left; timers top right.
- Floor work (yoga, pilates) needs the full width: camera landscape, `box` at `{"x": 0.5, "width": 0.9}` and only small graphics.

## Graphics

```
add_layer {"kind": "template", "template": "interval-timer", "params": {"work": 40, "rest": 20, "rounds": 8, "label": "HIIT"}, "visible": false}
add_layer {"kind": "template", "template": "steps", "params": {"steps": ["Squats", "Push-ups", "Lunges", "Plank"], "current": 1, "layout": "vertical"}, "x": 0.78, "y": 0.3, "w": 0.17, "h": 0.5, "visible": false}
add_layer {"kind": "template", "template": "breathing", "params": {"title": "Breathe with me", "inhale": 4, "hold": 2, "exhale": 6, "cycles": 6}, "x": 0.62, "y": 0.2, "w": 0.3, "h": 0.53, "visible": false}
add_layer {"kind": "template", "template": "lower-third", "params": {"title": "Jess Okafor", "subtitle": "Certified yoga and HIIT instructor"}, "hide_after_s": 15}
add_layer {"kind": "template", "template": "stopwatch", "params": {"label": "Plank hold"}, "visible": false}
```

## Steps

1. `status`, `start` if stopped; background, webcam; `snapshot` with the instructor standing and lying down to check nothing is cut off.
2. Warm-up (5 min): lower third, the exercise list (steps, vertical).
3. Main block: show interval-timer; advance the list as exercises change: `update_layer {"id": "steps", "params": {"current": 2}}`. New block: `update_layer {"id": "interval-timer", "params": {"work": 30, "rounds": 6, "label": "Core"}}` (restarts it).
4. Holds and challenges: stopwatch instead of the interval timer, never both.
5. Cool-down: hide timers, show breathing (it runs by itself). Save looks with `save_scene` ("warm-up", "workout", "cool-down") and switch with `load_scene`.
6. Close: social handles or the next class time for a minute, then `clear_layers`.

## Rules

- The instructor's face and full body must stay visible: graphics only in the free corner; check with `snapshot` after each change.
- One moving element at a time: the timer runs, everything else is still.
- Big, readable numbers; 5% safe margin.
- No medical claims in graphics.
- Clear graphics at the end.
