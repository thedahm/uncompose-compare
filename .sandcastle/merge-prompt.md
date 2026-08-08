# TASK

Merge the following branches into `{{SPEC_BRANCH}}`, each via a pull request so the work leaves a paper trail on GitHub:

{{BRANCHES}}

The PR is the merge vehicle, not just paperwork: `{{SPEC_BRANCH}}` only advances through `gh pr merge --merge`, so every landed branch shows up as a real merge commit on GitHub. Never `git merge` a branch into `{{SPEC_BRANCH}}` yourself and never push `{{SPEC_BRANCH}}` directly to land a branch — the one exception is the final fix-up step at the end.

For each branch, one at a time:

1. Push it: `git push -u origin <branch>`
2. Open a PR: `gh pr create --base {{SPEC_BRANCH}} --head <branch> --title "<issue title>" --body "<one-paragraph summary of what the branch does, referencing its issue like #42>"`
3. If you need to validate or the PR is not mergeable because of conflicts, work on the branch side only:
   - `git checkout <branch>` then `git merge {{SPEC_BRANCH}} --no-edit`
   - Resolve any conflicts intelligently by reading both sides and choosing the correct resolution
   - Run the repo checks (below); fix failures before continuing
   - `git push` (the branch — not `{{SPEC_BRANCH}}`)
4. Merge the PR with a merge commit: `gh pr merge <branch> --merge`
5. After the PR merges, `git checkout {{SPEC_BRANCH}} && git pull` so the next branch merges against the latest state

After all branches are merged, run the repo checks on `{{SPEC_BRANCH}}` one final time. If something fails, fix it, commit, and push — this post-merge fix-up is the only direct push to `{{SPEC_BRANCH}}` allowed.

# REPO CHECKS

`npm run typecheck` and `npm run test` at the repo root — they cover the frontend typecheck/build and `cargo test`. If the touched code has other documented checks (e.g. the packaging scripts under `scripts/`), run those too.

# CLOSE ISSUES

PRs merging into `{{SPEC_BRANCH}}` do not auto-close issues (GitHub only does that against the default branch), so close each merged branch's issue explicitly:

`gh issue close <ID> --comment "Completed by Sandcastle in <PR URL>"`

Here are all the issues:

{{ISSUES}}

Once you've merged everything you can, output <promise>COMPLETE</promise>.
