# Template catalogue

Add one with `add_layer {"kind": "template", "template": "<id>", "params": {…}}`; change values live with `update_layer {"id": "<layer id>", "params": {…}}`. Every template also takes `theme` (brand, news, minimal) and `accent` (#rrggbb). Full parameters: `guide` with the template id.

## Basics

- `caption-bar` Caption bar: Manual captions: up to two lines of large white text on a dark band, centred near the bottom, readable on any background.
- `confidential-badge` Confidential badge: A bordered CONFIDENTIAL (or any) warning badge for the top right, with an optional second line.
- `disclaimer-bar` Disclaimer bar: A thin full-width bar along the bottom edge with one line of small print, e.g. a disclaimer or notice.
- `live-badge` Live badge: A LIVE, REC or ON AIR pill with a slowly pulsing dot, for a top corner.
- `logo-bug` Logo bug: A logo or small picture in a corner, slightly see-through like a TV channel bug.
- `lower-third` Lower third: Name and role (or any two lines) in a band at the bottom left, with an optional logo.
- `name-badge` Name badge: A compact name tag for a corner: the name and a short line such as pronouns or role.
- `social-handles` Social handles: Where to find you: one handle per line (or in a row), each with a short coloured platform label.

## Time

- `clock` Clock: The current time, local or at a fixed UTC offset, 24h or 12h, with an optional label such as a city.
- `countdown` Countdown: Counts down to a time of day or date, or for a duration from when it is shown; shows a message at zero.
- `interval-timer` Interval timer: Fitness intervals: WORK and REST phases for a number of rounds, with the round, the time left and a phase colour; shows Done at the end.
- `speaker-timer` Speaker timer: A per-speaker countdown: turns amber near the end, red at zero, then counts the overtime as +m:ss.
- `stopwatch` Stopwatch: Counts up from when it is shown; can be paused and resumed without losing the time, or restarted from a given offset.

## Full-frame cards

- `be-right-back` Be right back (full frame): Full-frame break card on a calm, slowly drifting gradient: title, subtitle and an optional countdown.
- `section-title` Section title (full frame): Full-frame chapter heading: a big title with an optional number such as 01 and a small kicker line above it.
- `speaker-intro` Speaker intro (full frame): Full-frame speaker introduction: photo (or initials), name, role, company and a short bio.
- `starting-soon` Starting soon (full frame): Full-frame card before the show: title, subtitle and a countdown.
- `thank-you` Thank you (full frame): Full-frame closing card: a big thank-you, a line under it, optional social handles and an optional QR code for a link.

## Lists and text

- `agenda` Agenda: A numbered agenda with the current item highlighted and earlier items ticked; move `current` as the session goes on.
- `checklist` Checklist: A checklist with ticked and open items and a done count; tick items off live (release checklist, preflight, to-do list).
- `lyrics-card` Lyrics card: Centred lines of lyrics, a verse or scripture in the lower part of the picture, with an optional reference line (song title, John 3:16).
- `now-playing` Now playing: The song that is playing: title, artist, optional cover picture and an optional progress bar.
- `question-card` Question card: An audience question for Q&A: a tag, the question, and who asked it and where (e.g. via chat).
- `quote-card` Quote card: A quotation with a large quotation mark, the author and their role; for testimonials, famous quotes and pull quotes.
- `recipe-card` Recipe card: A recipe panel for cooking shows: title, servings and time, the ingredients list and the step being done right now.
- `steps` Steps: Numbered steps in a row or a column: the current step is highlighted, earlier ones are ticked. Good for tutorials, onboarding and processes.

## Data

- `bar-chart` Bar chart: Labelled bars from a table of label/value rows; changing values animates the bars.
- `comparison-table` Comparison table: Compares two or three options feature by feature; yes/no cells become check marks and crosses, one column can be highlighted.
- `goal-bar` Goal bar: A fundraising or follower goal: a bar that fills towards the goal, the amount raised and "62% of $10,000"; raising the amount animates the bar.
- `kpi-cards` KPI cards: 2 to 4 KPI tiles with a big value and a green/red change arrow; also the metrics board for software companies (MRR, DAU, uptime, latency). Numeric values animate on change.
- `leaderboard` Leaderboard: Ranked names and scores, sorted highest first, with gold, silver and bronze for the top three; changed scores count up and the order follows.
- `line-chart` Line chart: A line chart with a shaded area from label/value rows, the latest value shown big; changed values animate.
- `poll-results` Poll results: Live poll results: the question and one bar per option with its percentage, the leader highlighted; new vote counts animate.
- `progress-donut` Progress donut: A ring that fills to value out of max, with the percentage (or your own text) in the centre and a label; changes animate.
- `scoreboard` Scoreboard: A score bug for two teams or players: names, team colours, scores, the period and an optional clock text; a changed score animates with a soft glow.

## Call to action

- `product-card` Product card: Live-shopping card: product picture, name, price with the old price struck through, a discount badge, an optional offer countdown and a stock bar.
- `qr-card` QR card: A QR code with a heading and a short line: a link, a sign-up, a download.
- `sponsor-carousel` Sponsor carousel: Thanks sponsors one at a time: each logo (or name) fades in, stays a few seconds, then gives way to the next.

## Calm

- `breathing` Breathing guide: A calm circle that slowly grows while breathing in, rests while holding and shrinks while breathing out, with the phase written inside.

## Software companies

- `architecture-diagram` Architecture diagram: Boxes and arrows laid out automatically: nodes in numbered columns from left to right, edges as arrows with optional labels.
- `callout` Callout: An arrow with a label that points at a spot on a shared screen or demo, with an optional softly pulsing ring at the tip.
- `code-card` Code card: A syntax-highlighted code snippet in an editor-style card, optionally typed out.
- `keyboard-shortcut` Keyboard shortcut: Key caps joined by + (e.g. Ctrl + Shift + P) with an optional caption saying what the shortcut does.
- `release-notes` Release notes: "What's new" panel for a version: date and three optional lists (new, improved, fixed) under coloured labels.
- `roadmap-timeline` Roadmap timeline: Milestones on a horizontal timeline: done ones filled, the current one highlighted, next and later ones outlined.
- `status-card` Status card: Service status panel: a coloured overall status, the incident title, a timeline of updates (newest first) and optional per-component states.
- `terminal-card` Terminal: A terminal window with a prompt, a command and its output; can type the command out and then reveal the output line by line.
- `version-badge` Version badge: A badge announcing a release: a small "Now shipping" kicker, the version in large type and an optional tagline.
