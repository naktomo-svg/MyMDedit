---
name: why5-and-horizontal-check
description: Use when about to fix a confirmed-real bug or tech-debt finding (already verified against current code) — before writing the fix, to trace its root cause and find every sibling occurrence of the same mistake in the codebase.
---

# Why5 And Horizontal Check

## Overview

A symptom-level patch leaves the root cause, and every sibling occurrence of the same mistake, in place. Trace to the root cause, then search the whole codebase for the same pattern, before writing any fix.

**REQUIRED BACKGROUND:** Only run this on findings verify-then-fix already classified ✅本物.

## Steps

1. State the symptom in one line.
2. Ask "why?" about 5 times, one line per step, each answer chained from the previous. Stop early if the true root cause is reached before step 5; keep going past step 5 if it's still surface-level.
3. Horizontal check — decompose the root-cause pattern into two independent axes and grep each separately, then combine:
   - **動詞 axis**: what operation is happening (e.g. `let _ =`, `.ok()`, `borrow_mut()`, `as *const`).
   - **受け手 axis**: what it's operating on (e.g. `Result`, `state`, a Win32 buffer or a specific field name).
   - Grep each axis independently with the `Grep` tool and union the hit counts — a single combined regex misses occurrences that phrase the same mistake differently on one axis but not the other.
4. Pick a fix that covers the root cause and every occurrence found in step 3, not just the original symptom site.
5. Pick a reproduction-proof safeguard, in this priority order: a clippy lint enabled in `Cargo.toml`'s `[lints.clippy]` (when one exists for the pattern) > an automated test (`#[cfg(test)]` in the module; this repo's pure-logic modules are unit-tested) > a written note in CLAUDE.md (last resort only, when neither can express the check — typically Win32/WebView2 behaviour that only shows at runtime).

## Done means

Run every command CLAUDE.md's Commands section lists for verification and confirm each meets the bar stated there. Do not invent commands it does not list. When the fix touches UI behaviour, also do a manual QA pass with the app.

## Red flags — you're skipping a step

| Excuse | Reality |
|---|---|
| "The fix is obvious, no need for 5 whys" | Obvious fixes patch symptoms. Trace to the root cause anyway. |
| "I already know where else this pattern shows up" | Confirm with a real grep — memory misses occurrences. |
| "One grep covering both axes is faster" | A single regex misses the same mistake phrased differently on one axis. Run both, separately. |
| "Writing it in CLAUDE.md is good enough" | CLAUDE.md is the last resort, only when a lint or test genuinely can't express the check. |
| "We're under time pressure, just patch the one spot" | The sibling occurrences don't go away under time pressure — they just resurface later as separate bug reports. |
