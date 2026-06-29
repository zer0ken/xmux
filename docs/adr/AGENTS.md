# Working Notes: /docs/adr

## Purpose

`docs/adr/` records accepted architecture and documentation decisions.

## Mental Model

An ADR explains a decision that should guide contributors. It is not a task log,
release note, or scratchpad.

## Module Seams

- One ADR should cover one decision.
- Cross-link related public docs when the decision changes how contributors
  should edit the repository.

## Invariants

- ADRs are written in English.
- Decision text should be stable and actionable.
- Consequences should state the repository rule created by the decision.

## Common Pitfalls

- Do not use ADRs for temporary task lists.
- Do not record implementation history as the main value of a decision.

## Before Editing

- Check whether an existing ADR already covers the rule.
- Keep new entries focused on a decision, not a broad design essay.

## Verification

- Check links and filenames.
- Confirm any repository rule stated here is reflected in nearby Working Notes
  or public docs.
