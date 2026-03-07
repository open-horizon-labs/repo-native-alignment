---
id: agents-need-write-for-sessions
outcome: agent-alignment
severity: hard
statement: Phase agents that persist to .oh/ session files must have Write in their tools list
---

Every OH phase agent reads/writes `.oh/<session>.md` for context handoff. Without `Write` in frontmatter tools, the agent runs but silently can't persist output. This bit us three times before we caught it — oh-solution-space, oh-problem-space, and oh-aim all failed to write session files.

## Override Protocol
Read-only agents (if any exist in future) can omit Write. All current phase agents need it.
