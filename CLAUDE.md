@AGENTS.md

## Claude Code only

`AGENTS.md` (imported above) holds every instruction shared with other agents. This file adds only what needs Claude Code.

- Skills in `.claude/skills/` are invoked with the Skill tool.
- Agent team (coordinator = main session, `implementer` and `reviewer` subagents in `.claude/agents/`, git worktrees under `.claude/worktrees/`, guard hooks in `.claude/hooks/`): the workflow and rules are in the file imported below.

@CLAUDE.team.md
