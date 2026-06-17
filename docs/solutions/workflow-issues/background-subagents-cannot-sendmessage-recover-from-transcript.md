---
title: "Background named subagents can't SendMessage back — recover reports from the per-agent transcript"
date: 2026-06-17
category: workflow-issues
module: agent-orchestration
problem_type: workflow_issue
component: development_workflow
severity: medium
applies_when:
  - "dispatching parallel subagents via the Agent tool with run_in_background:true and a name (persistent named teammates)"
  - "a spawned subagent reports SendMessage is \"No such tool available / not enabled in this context\""
  - "a finished background agent emits only an idle_notification and never its report content"
  - "you need to recover a subagent's output after the messaging channel back to the orchestrator fails"
tags:
  - orchestration
  - subagents
  - sendmessage
  - background-agents
  - transcript-recovery
  - parallel-review
  - agent-tool
---

# Background named subagents can't SendMessage back — recover reports from the per-agent transcript

## Context

When an orchestrator (a main Claude Code session) runs a parallel multi-agent review, it may dispatch reviewer subagents via the `Agent` tool with `run_in_background: true` plus a `name` — which makes them persistent named "teammates." In that configuration, when a background subagent finishes its one-shot task the only thing that reaches the orchestrator is a teammate signal:

```json
{"type":"idle_notification","from":"<name>","idleReason":"available"}
```

The report content never arrives. Sending the agent a `SendMessage` request to "send your report to main" produces only another `idle_notification`, because the spawned subagents do **not** have the `SendMessage` tool available ("No such tool available / not enabled in this context"). A plain-text reply from such a subagent is not routed to main — delivery requires the subagent to call `SendMessage(to="main")`, which it cannot. The agents instead leave their report as their final assistant message in their own transcript, or `Write` it to a file. `TaskOutput` keyed by agent name or agent_id returns `No task found`, so it is not a recovery path either.

The cost is wasted orchestrator turns spent on futile `SendMessage`/`TaskOutput` round-trips, and the substantive output of several expensive reviewer agents sitting unread.

## Guidance

The gap is specific to **`run_in_background: true` + a `name`**. Decide the delivery mechanism *before* dispatching, not after seeing `idle_notification`.

**Option A (preferred for collect-and-synthesize): use foreground agents.**
A foreground `Agent` call — no `run_in_background`, no `name` — returns the subagent's final assistant message directly as the tool result. No messaging hop, no transcript spelunking. Issue several in one message to run them concurrently and read their returned text. This is the default for review/audit fan-out where the reports are the deliverable.

**Option B (for genuinely long background fan-out): mandate a known output path up front.**
If the agents must run in the background, instruct each one in its dispatch prompt to write its report to a predictable file and not rely on messaging:

```
When finished, Write your full report to tmp/review-<your-scope>.json.
Do NOT rely on SendMessage — it is not available to you.
```

On the `idle_notification`, read `tmp/review-*.json`. Do not attempt `SendMessage` round-trips.

**Recovery (reports already stranded, no path was mandated): extract from session transcripts.**
Each subagent runs in its own session whose transcript is a JSONL file at `~/.claude/projects/<PROJECT-slug>/<uuid>.jsonl` — one per subagent, recently modified. Exclude the orchestrator's own session uuid. Take the last `type=="assistant"` line and concatenate its text parts; disambiguate which file is which agent by matching the first `user` message (the dispatch prompt) against the agent's scope marker.

```python
import json, glob, os

TRANSCRIPT_DIR = os.path.expanduser("~/.claude/projects/<PROJECT-slug>")
ORCHESTRATOR_UUID = "<orchestrator-session-uuid>"  # exclude main's own transcript

def text_parts(msg):
    c = msg.get("content", "")
    if isinstance(c, str):
        return c
    return "".join(p.get("text", "") for p in c
                   if isinstance(p, dict) and p.get("type") == "text")

for path in sorted(glob.glob(os.path.join(TRANSCRIPT_DIR, "*.jsonl")),
                   key=os.path.getmtime, reverse=True):
    if ORCHESTRATOR_UUID in os.path.basename(path):
        continue
    rows = [json.loads(l) for l in open(path, encoding="utf-8") if l.strip()]
    first_user = next((text_parts(r["message"]) for r in rows
                       if r.get("type") == "user" and "message" in r), "")
    last_asst = next((text_parts(r["message"]) for r in reversed(rows)
                      if r.get("type") == "assistant" and "message" in r), "")
    print("FILE:", os.path.basename(path))
    print("MARKER:", first_user[:200].replace("\n", " "))
    print("REPORT:\n", last_asst, "\n", "=" * 60)
```

Stdlib only. Claude Code transcript lines nest the model message under a `message` key; adjust the unwrap if a line stores content at top level.

## Why This Matters

Without a chosen delivery mechanism, the orchestrator believes it has nothing while the reports sit in transcripts or `tmp/` files, and turns are burned on retrieval attempts that cannot return content. Option A makes the report the tool result at zero retrieval cost; Option B reduces retrieval to one predictable `read`; transcript extraction is the deterministic last resort that recovers stranded reports instead of re-running the agents.

## When to Apply

- Orchestrating parallel multi-agent reviews/audits where each agent's structured report is needed.
- Any reach for `Agent` with `run_in_background: true` **and** a `name` — that combination triggers the gap.
- Collecting structured or long reports from subagents (as opposed to fire-and-forget background work).

## Examples

Before — background named agent, content never delivered:

```
Agent(name="rev", run_in_background=true, prompt="review X")
→ orchestrator receives only {"type":"idle_notification","from":"rev"}
→ SendMessage(to="rev", "send report")  → agent: "No such tool available" → idle again
→ TaskOutput(task_id="rev")             → "No task found"
```

After — option A (foreground returns the report directly):

```
result = Agent(subagent_type="reviewer", prompt="review X")   # no name, no run_in_background
# result IS the full report
```

After — option B (background + mandated path):

```
Agent(name="rev", run_in_background=true,
      prompt="review X. When done, Write your report to tmp/review-x.json; SendMessage is unavailable.")
# on idle_notification: read tmp/review-x.json
```

## Related

- Distinct from the xmux product docs under `docs/solutions/` — this is a Claude Code orchestration-workflow learning, not a code pattern. No overlap with the existing architecture/tooling/ui entries.
