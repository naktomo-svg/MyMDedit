---
name: tech-debt-report
description: Use when generating, resuming, or interpreting this repo's AI tech-debt report via scripts/Analyze-TechDebt.ps1 — covers the flags, the clippy section, the caching/resume behaviour that makes an interrupted run cheap to restart, the expected sandbox kill, and where the output lands.
---

# Tech-debt report

Generate an AI tech-debt report (makes real `claude` API calls; defaults to Opus for both the batch review and the meta-analysis pass, so a full run costs materially more than a few tens of cents; `-DryRun` prints the batch plan and the clippy diagnostic count without calling the API):

```powershell
pwsh ./scripts/Analyze-TechDebt.ps1 [-DryRun] [-Fresh] [-SkipClippy] [-ReviewModel opus] [-ReviewEffort high] [-MetaModel opus] [-MetaEffort medium]
```

Two independent sources feed the report:

- **AI review** — the Rust sources, `Cargo.toml` and `res/` (excluding `target/`), sent in batches to `claude --print`.
- **Static analysis** — `cargo clippy --all-targets`, run locally at no API cost. Its diagnostics are written verbatim to the report's 「静的解析(clippy)」 section and passed to the meta-analysis so it can merge them with AI findings that share a root cause. `-SkipClippy` omits it; if clippy cannot run, the section says so rather than reading as "clean".

**A full run is expected to be killed by this sandbox's process-lifetime limit before it finishes — that is not a sign the script is broken.** Batch and meta results are cached (keyed by content + prompt + model + effort + `$ClaudeCommand`; the meta key also covers the clippy output) under `docs/tech-debt/.partial/` as each step completes; just re-run the same command and it resumes, only re-paying for the steps that hadn't finished. `-Fresh` bypasses the cache and forces every step to re-run. `docs/tech-debt/.partial/` is gitignored and safe to delete at any time (worst case, the next run re-does more work). If a run finishes with some steps still failed, the meta-analysis is skipped and the report is written to `docs/tech-debt/<date>-partial.md` instead of overwriting the canonical `<date>.md`/`latest.md`.

Writes `docs/tech-debt/<date>.md` plus a `docs/tech-debt/latest.md` copy on a fully successful run.

## Acting on the findings

Before acting on a finding from this report — AI or clippy alike — invoke the `verify-then-fix` skill; before fixing a confirmed-real finding, invoke `why5-and-horizontal-check`.
