---
name: day
description: Show today's checklist, the next gate, and the time left to the ETHOnline deadline from plan.md; mark items done when told. Use at the start of a session and after finishing a task.
argument-hint: [done <text-fragment>]
---

`plan.md` lives at `/Users/vicky/Desktop/dev/hanvil/plan.md` (local, gitignored).

- With no arguments: print today's date (IST), hours remaining to Sun 2026-09-13 21:30 IST, the
  section of `plan.md` §10 for today with its checkboxes, the next gate (G1/G2/G3) and its
  fallback, and the next check-in deadline. Nothing else.
- With `done <fragment>`: find the single unchecked `- [ ]` line in `plan.md` containing the
  fragment, flip it to `- [x]`, and print the line. If zero or several match, print them and
  change nothing.
