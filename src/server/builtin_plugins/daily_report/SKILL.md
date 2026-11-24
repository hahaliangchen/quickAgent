---
name: daily-report
description: Read the team's daily-report writing guide, gather today's real work, and hand a reviewed draft to the company daily-report system.
---

# Team daily report

This plugin writes the internal office system requirement: every workday one daily report with one or more projects. The report is submitted to the company server, so it must contain only facts you can point at.

## What to do when a user asks for a daily report

1. `daily_report.read_guide({})`. Follow the guide it returns. When it is empty, say so and ask for the house style instead of inventing one.
2. `daily_report.today_activity({})` for the real material: sessions, changed files and commits from today. Never invent work that is not in that output, and never pad with generic verbs.
3. `daily_report.list({})` for the project list and recent reports. Reuse the exact project name from the server; a misspelled name creates a duplicate project entry.
4. `daily_report.prepare_draft({...})` with one `projects[]` entry per project worked on, `work_content` ordered as the guide asks. This stores the draft and opens the page in the workbench.
5. Submit only after the user confirms: `daily_report.submit({})`. It always raises a one-shot approval card, and refusing is the normal outcome when the draft is wrong.

## Rules

- Never guess hours, overtime or reimbursement. Leave them at 0 unless the user states them.
- A report date already present on the server is an update (`action: "update"`), not a second report.
- The access token and password stay on the host. Ask the user to log in from the workbench page when a tool reports `AUTH_EXPIRED`.
- `daily_report.write_guide` is for distilling a style from real history with the user, not for editing it silently.
