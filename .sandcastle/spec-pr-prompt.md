<!-- sandcastle-kit cd27a04 — synced copy, edit in sandcastle-kit -->

# CONTEXT

Every sub-issue of spec issue #{{SPEC_ISSUE}} ("{{SPEC_TITLE}}") has been implemented and merged into `{{SPEC_BRANCH}}`. Your job is to open the pull request that delivers the spec to `main`. A human will review and merge this PR — write it for them.

# TASK

1. Read the spec: `gh issue view {{SPEC_ISSUE}} --comments`
2. Survey what landed: `git log main..{{SPEC_BRANCH}} --oneline` and `gh pr list --base {{SPEC_BRANCH}} --state merged --json number,title`
3. Read enough of the changed code (`git diff main...{{SPEC_BRANCH}} --stat`, then the key files) to describe the change accurately and to write honest manual-test steps
4. Push the branch: `git push -u origin {{SPEC_BRANCH}}`
5. Open the PR: `gh pr create --base main --head {{SPEC_BRANCH}}` with the title and body below

# PR REQUIREMENTS

**Title**: human and merge friendly. Describe the capability delivered in plain words. No branch names, no "sandcastle", no bare issue numbers.

**Body**, in this order, kept scannable — useful without being overwhelming:

- **Summary** — two or three sentences: what this delivers and why
- **What changed** — short bullets grouped by area, not a commit list
- **How to test manually** — concise numbered steps a human can actually run from a fresh checkout: the commands to run and what to look for. Cover only the meaningful paths.
- **Notes** — tradeoffs, deferred work, follow-up issues; include this section only if there is something real to say
- End with `Closes #{{SPEC_ISSUE}}`

# OUTPUT

When the PR is open, output its number as JSON wrapped in `<pr>` tags:

<pr>{"number": 123}</pr>
