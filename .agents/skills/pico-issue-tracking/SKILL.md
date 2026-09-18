---
name: pico-issue-tracking
description: Implement and track PicoDataLogger GitHub lesson issues in small, learner-friendly slices. Use when starting, implementing, continuing, reviewing, or asking what is next for a project issue.
---

# PicoDataLogger issue tracking

Use the GitHub lesson issue as the scope and learning contract for implementation work. Follow `AGENTS.md` and preserve the wiring in `README.md`.

Maintain one authoritative issue comment containing `<!-- pico-data-logger-issue-tracker -->`. A request to start, implement, continue, review, or report progress on an issue authorizes creating or updating that tracker comment. It does not authorize closing the issue or rewriting its curriculum body.

## Start or resume an issue

1. Read the issue body, labels, milestone, dependencies, and all comments with `gh issue view` or the equivalent GitHub API.
2. Inspect the current repository and relevant upstream documentation. Check dependency issues rather than assuming their state from links alone.
3. Find the comment containing the tracker marker. Reuse it if present; never create a second authoritative tracker.
4. Build or refresh a tracker covering the whole issue while keeping the user's requested slice as the current position.
5. Mark existing work complete only when repository or hardware evidence supports it.

Use this tracker shape:

```markdown
## Issue #<number> implementation tracker

<!-- pico-data-logger-issue-tracker -->

**Goal:** <short outcome from the issue>

### Dependency readiness
- [ ] #<number> - <dependency and evidence needed>

### Implementation
- [ ] <small ordered task>

### Verification
- [ ] <build, test, or manual checkpoint>

### Learning check
- [ ] <issue's explain-it-back question; learner-owned>

**Current position:** <first actionable unchecked step>
```

Keep tasks small enough to review individually. Preserve the issue's intended behavior, technical decisions, and acceptance checkpoint; do not invent unrelated production hardening.

## Implement a requested slice

1. State the concept being exercised and the concrete slice being implemented.
2. Make the smallest coherent change that advances the active tracker step. Do not silently implement future issues.
3. Add or update tests appropriate to the slice. For hardware-only behavior, provide a precise manual check and expected observation.
4. Run formatting, focused tests, cross-builds, or static checks proportional to the change.
5. Review the resulting diff for scope, secrets, pin assignments, fixed-memory assumptions, and accidental desktop-only dependencies.
6. Edit the existing tracker comment: check only verified work, record concise evidence beside the relevant item when useful, and move `Current position` to the next actionable step.
7. Return the direct issue or tracker-comment link, what changed, verification performed, the next learning step, and any hardware observation still needed from the user.

Use explanations to connect the change to Rust ownership, traits, `Result`, async execution, static memory, I2C, networking, or MQTT as relevant. Keep those explanations focused on code the learner can currently see.

## Review or progress requests

When asked to review work or report progress:

1. Treat the tracker as an index, not proof.
2. Inspect the implementation, diff, tests, and required hardware evidence.
3. Report concrete defects before checking corresponding items.
4. Update the same tracker only when evidence changes its state.
5. Keep incomplete, failing, stubbed, untested, and learner-owned reflection items unchecked.
6. If nothing newly qualifies, leave the tracker unchanged and explain why.

## Completion boundaries

- A successful cross-build does not prove USB, Wi-Fi, I2C, sensor, or MQTT behavior on the physical device.
- User-reported serial output or broker observations may satisfy a manual checkpoint; summarize that evidence in the tracker.
- Never mark an explain-it-back item complete on the learner's behalf.
- Never expose Wi-Fi or MQTT credentials in issue comments, logs, commands, or committed files.
- Do not close the issue automatically. When every implementation and verification item is complete, report that it is ready to close and identify any remaining learning checks separately.
