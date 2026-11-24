---
title: Privacy and compliance rules
aliases: compliance, confidential, gdpr, hipaa, patient data, private, safety, rules
---
# Privacy and compliance rules

Everything Page2Cam shows goes live to the people in the call and to anyone recording or watching the stream.

**Everyone**
- Never put private details on screen unless the user asks for that exact text: home addresses, phone numbers, emails, account numbers, passwords, tokens, internal URLs.
- `set_background` with `screen` shares a whole monitor, notifications and all: tell the user before choosing it and suggest closing private windows.
- Prefer the blurred-room background when the room behind the user is private (a home, a clinic, an office with whiteboards).
- Clear graphics at the end (`clear_layers`) so the next call doesn't start with old ones.
- Stream keys are never readable; `start_stream` uses the destination saved in the control page.

**Medical and therapy**
- Never show patient names, dates of birth, diagnoses, medications, test results or case notes in a graphic, even if the user mentions them in the chat.
- Use the clinician's name and role only (lower-third or name-badge), the practice name and generic information (opening hours, "this session is private").
- Prefer `blurred-room`. Therapy: no flashing, no busy motion, no tickers; calm templates (breathing) only when the client wants them.

**Legal and consulting**
- No client names, case numbers or matter details on screen. Add `confidential-badge` when the user asks for it or the meeting is marked confidential.

**Finance**
- No account numbers, balances or client holdings. Public market data only, with its source.
- While streaming or recording publicly, keep a `disclaimer-bar` visible the whole time (e.g. "For information only. Not financial advice. Capital at risk.").

**Children and classrooms**
- No pupils' full names or photos in graphics; first names only when the teacher asks.

When unsure, ask the user before showing something.
