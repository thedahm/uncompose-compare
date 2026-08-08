# TASK

Perform the final review of PR #{{PR_NUMBER}} (`{{SPEC_BRANCH}}` → `main`), which delivers spec issue #{{SPEC_ISSUE}}, and post your findings as a comment on the PR.

# HOW TO REVIEW

**Skill-first**: if the `code-review` skill is available in this environment (it reviews the changes since a fixed point along two axes, Standards and Spec), invoke it with `main` as the fixed point and issue #{{SPEC_ISSUE}} as the spec source, and use its output as your findings.

**Fallback**: if the skill is not available, run the same two-axis review yourself:

- Pin the diff: `git diff main...{{SPEC_BRANCH}}` and `git log main..{{SPEC_BRANCH}} --oneline`
- **Standards axis** — does the diff conform to the repo's documented standards (`.sandcastle/CODING_STANDARDS.md`, `CLAUDE.md`, anything else in the repo that documents how code should be written)? Also flag classic code smells (mysterious names, duplicated code, feature envy, data clumps, primitive obsession, shotgun surgery, speculative generality) — always as judgement calls, never hard violations, and a documented repo standard overrides a smell. Skip anything tooling already enforces.
- **Spec axis** — fetch issue #{{SPEC_ISSUE}} with its comments and sub-issues. Report requirements that are missing or partial, behaviour the spec didn't ask for, and requirements that look implemented but wrong. Quote the spec line for each finding.

Keep the two axes separate; do not merge or rerank findings across them.

# POST

Post exactly one comment on the PR via `gh pr comment {{PR_NUMBER}} --body "..."`, structured as:

- Header: `## Final review (fable-5)`
- `### Standards` and `### Spec` sections, each finding tagged with a severity (blocking / significant / minor / nit) and citing the file/line or spec line
- If an axis is clean, say so explicitly
- One-line closing summary: finding counts per axis and the worst issue within each

Once the comment is posted, output <promise>COMPLETE</promise>.
