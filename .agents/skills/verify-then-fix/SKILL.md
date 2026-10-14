---
name: verify-then-fix
description: Use when about to act on a finding from a tech-debt report, clippy, code review, or any external/automated audit — before editing code to address a claimed bug or issue in this repository.
---

# Verify Then Fix

## Overview

Findings from AI-generated tech-debt reports, code reviews and lints are false-positive-prone, and can describe bugs that a later commit already fixed. Verify a finding against the real, current code before touching anything.

## Verification steps

1. Read the exact file:line the finding cites (`Read`/`Grep`) — confirm the described behavior exists in the code today, not just in the finding's prose. Line numbers drift between the report's commit and now; locate the code by content if the line no longer matches.
2. Reproduce it: run the test command from AGENTS.md's Commands section if a test covers this path, or write the smallest failing unit test when the code is a pure function (`md_ops`, `search`, `document`, `line_endings`, `settings`, `render`). Code that only exists behind Win32/WebView2 calls has no test harness — trace the logic by hand or do a manual QA step with the app.
3. Check `git log -p` / `git blame` on the cited lines — the finding may already be fixed by a commit later than whatever the report was generated against (the report header records its source SHA).
4. Classify the finding using exactly one of:
   - ✅ 本物 (confirmed real, as described)
   - 🟡 主張が一部誤り・降格 (partially wrong — a real issue exists, but not as severe/exact as claimed)
   - 🟠 本物だが意図的に対応しない (real, but a deliberate design choice — won't fix)
   - ⚪ 誤検知 (false positive — not reproducible in current code)
   - 🔁 解決済み (already fixed by a prior commit)

For clippy findings specifically: a lint can be correct about the pattern yet wrong about this context (e.g. `manual_dangling_ptr` on `PCWSTR(1 as *const u16)`, which is the Win32 `MAKEINTRESOURCE` idiom, not a pointer to anything). That is ⚪, and the fix is a scoped `#[allow(clippy::…)]` with a one-line reason — not a code change.

## What happens next

- ✅ only → **REQUIRED SUB-SKILL:** Use why5-and-horizontal-check to find the root cause and fix it.
- 🟡🟠⚪🔁 → do not edit code for this finding (other than a justified `#[allow]` for a ⚪ lint). Write one line stating the tag and why, then stop.

## Red flags — you're skipping verification

| Excuse | Reality |
|---|---|
| "The report already explains the bug in detail" | The report can't see today's code. Read the live file anyway. |
| "This looks obviously right" | Plausible-sounding findings are exactly the false-positive-prone kind. Read the code before trusting the prose. |
| "clippy said so, it must be right" | Lints match patterns, not intent. Win32 interop trips several of them legitimately. |
| "I'll fix it and verify after" | Fixing before verifying risks patching an already-fixed or non-existent bug — check `git log` first. |
| "It's a big batch of findings, verifying each is slow" | An unverified fix costs more later (reverts, wrong root-cause). Verify every one, even in a batch. |
| "We're under time pressure, just apply it" | Time pressure is exactly when unverified fixes cause the most damage. Verify first, every time. |
