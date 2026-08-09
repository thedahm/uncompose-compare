<!-- sandcastle-kit cd27a04 — synced copy, edit in sandcastle-kit -->

# TASK

Address the final review on PR #{{PR_NUMBER}}. The review is a PR comment headed `## Final review (fable-5)`. Work directly on `{{SPEC_BRANCH}}` — never on `main`. This is a single round: address the feedback, reply, done. Do not request or wait for a re-review.

# 1. FETCH

Pull all comment surfaces:

- `gh pr view {{PR_NUMBER}} --json reviews`
- `gh pr view {{PR_NUMBER}} --json comments`
- `gh api repos/{owner}/{repo}/pulls/{{PR_NUMBER}}/comments`

The comment headed `## Final review (fable-5)` is the review to address. A comment from the repo owner without that header is direct user input, not review feedback — treat it as an instruction and just do it.

# 2. TRIAGE

Break the review into discrete actionable items. Classify each: **blocking** (a real correctness or security problem, or the reviewer marked it fix-before-merge), **significant** (design, contract, docs), **minor**, or **nit**. Take the reviewer's severity tags as the starting point, but re-judge each against the code; a mislabeled item gets your classification, noted as such.

# 3. JUDGMENT

- Default: follow the reviewer. Most items just get done.
- Minors and nits: your call — adopt, adapt, or decline, with a one-line reason in the disposition.
- You may decline significant items when you're confident the suggestion doesn't fit the code as it actually is — but never silently: every decline gets its reasoning in the disposition comment.
- Never silently comply with something you believe is wrong; adapt it and say how.

# 4. ACT

Apply the adopted changes on `{{SPEC_BRANCH}}`. Before pushing, run the repo checks:

{{REPO_CHECKS}}

Commit with a message referencing the final review, then push.

# 5. REPLY

Post one comment on the PR with a per-item disposition table:

| Mark | Meaning |
|---|---|
| ✅ adopted | done as suggested |
| ☑️ adapted | done differently — say how and why |
| ✋ declined | not doing it — say why |

Tone: collegial and precise — the reply is the record of why the code is the way it is.

Once pushed and replied, output <promise>COMPLETE</promise>.
