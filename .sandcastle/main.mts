// Parallel Planner with Review — spec-delivery orchestration loop
//
// One run delivers one spec issue (label: "spec") by working its
// ready-for-agent sub-issues and, once they're all closed, opening a
// human-mergeable PR against main:
//   Phase 1 (Plan):             A fable agent analyzes open ready-for-agent
//                               issues, builds a dependency graph, and outputs
//                               a <plan> JSON listing at most MAX_PARALLEL
//                               unblocked issues with branch names.
//   Phase 2 (Execute + Review): For each issue, a sandbox is created via
//                               createSandbox(). The implementer runs first
//                               (100 iterations). If it produces commits, a
//                               reviewer runs in the same sandbox on the same
//                               branch (1 iteration). All issue pipelines run
//                               concurrently via Promise.allSettled().
//   Phase 3 (Merge):            A single agent merges each completed branch
//                               into the spec branch via a PR (opened and
//                               merged by the agent, for the paper trail).
//   Phase 4 (Finalize):         Once every GitHub sub-issue of the spec is
//                               closed: open the spec PR against main
//                               (spec-pr), post a final review as a PR
//                               comment (final-reviewer), then address that
//                               review in one round (address-final-review).
//                               The spec PR is left open for a human to merge.
//
// The outer loop repeats up to MAX_ITERATIONS times so that newly unblocked
// issues are picked up after each round of merges.
//
// Usage:
//   npx tsx .sandcastle/main.mts
// Or add to package.json:
//   "scripts": { "sandcastle": "npx tsx .sandcastle/main.mts" }

import { execSync } from "node:child_process";
import * as sandcastle from "@ai-hero/sandcastle";
import { docker } from "@ai-hero/sandcastle/sandboxes/docker";
import { z } from "zod";

// The planner emits its plan as JSON inside <plan> tags; Output.object extracts
// and validates it against this schema. We use Zod here, but any Standard
// Schema validator works just as well — Valibot, ArkType, etc. See
// https://standardschema.dev.
const planSchema = z.object({
  issues: z.array(
    z.object({ id: z.string(), title: z.string(), branch: z.string() }),
  ),
});

// The spec-pr agent emits the number of the PR it opened inside <pr> tags.
const prSchema = z.object({ number: z.number() });

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

// Maximum number of plan→execute→merge cycles before stopping.
// Raise this if your backlog is large; lower it for a quick smoke-test run.
const MAX_ITERATIONS = 10;

// Maximum number of issues worked in parallel per cycle. The planner is asked
// to select at most this many; the slice in the loop enforces it regardless.
const MAX_PARALLEL = Number(process.env.SANDCASTLE_MAX_PARALLEL ?? 2);

// Hooks run inside the sandbox before the agent starts each iteration.
// npm install ensures the sandbox always has fresh dependencies.
const hooks = {
  sandbox: { onSandboxReady: [{ command: "npm install" }] },
};

// Copy node_modules and the cargo target dir from the host into the worktree
// before each sandbox starts. Avoids a full npm install and a cold cargo
// build from scratch; the hook above handles platform-specific binaries and
// any packages added since the last copy.
const copyToWorktree = ["node_modules", "frontend/node_modules", "target"];

// ---------------------------------------------------------------------------
// Spec resolution
//
// One run delivers exactly one open issue labeled "spec". Sub-issue branches
// merge into the spec branch — today, the branch checked out when the run
// starts. (Multi-spec support would resolve a worktree + branch per spec
// here; everything downstream already keys off specBranch.)
// ---------------------------------------------------------------------------

const sh = (command: string) => execSync(command, { encoding: "utf8" }).trim();

const specBranch = sh("git rev-parse --abbrev-ref HEAD");
const repo = sh("gh repo view --json nameWithOwner --jq .nameWithOwner");

const specCandidates: { number: number; title: string }[] = JSON.parse(
  sh("gh issue list --state open --label spec --json number,title"),
);
if (specCandidates.length !== 1) {
  throw new Error(
    `Expected exactly one open issue labeled "spec", found ${specCandidates.length}. Multi-spec runs are not supported yet.`,
  );
}
const spec = specCandidates[0]!;
console.log(`Spec: #${spec.number} ${spec.title} → branch ${specBranch}`);

// The spec's open sub-issues, via GitHub's sub-issue relationship. This is
// the truth condition for finalization: the spec PR is cut only when this
// list is empty, so sub-issues added mid-run (or left open as
// ready-for-human) block the finalize phase.
const openSubIssues = (): {
  number: number;
  title: string;
  labels: { name: string }[];
}[] =>
  JSON.parse(
    sh(
      `gh api "repos/${repo}/issues/${spec.number}/sub_issues?per_page=100"`,
    ),
  ).filter((issue: { state: string }) => issue.state === "open");

// ---------------------------------------------------------------------------
// Main loop
// ---------------------------------------------------------------------------

for (let iteration = 1; iteration <= MAX_ITERATIONS; iteration++) {
  console.log(`\n=== Iteration ${iteration}/${MAX_ITERATIONS} ===\n`);

  // -------------------------------------------------------------------------
  // Phase 1: Plan
  //
  // The planning agent (opus, for deeper reasoning) reads the open issue list,
  // builds a dependency graph, and selects the issues that can be worked in
  // parallel right now (i.e., no blocking dependencies on other open issues).
  //
  // It outputs a <plan> JSON block — Output.object parses and validates it.
  // -------------------------------------------------------------------------
  const plan = await sandcastle.run({
    hooks,
    sandbox: docker(),
    name: "planner",
    // One iteration is enough: the planner just needs to read and reason,
    // not write code. (Structured output requires maxIterations: 1.)
    maxIterations: 1,
    // Opus for planning: dependency analysis benefits from deeper reasoning.
    agent: sandcastle.claudeCode("claude-fable-5"),
    promptFile: "./.sandcastle/plan-prompt.md",
    promptArgs: { MAX_PARALLEL: String(MAX_PARALLEL) },
    // Extract and validate the <plan> JSON into a typed object. Throws
    // StructuredOutputError if the tag is missing, the JSON is malformed, or
    // validation fails — which aborts the loop.
    output: sandcastle.Output.object({ tag: "plan", schema: planSchema }),
  });

  // The prompt asks for at most MAX_PARALLEL issues; enforce it here too.
  const issues = plan.output.issues.slice(0, MAX_PARALLEL);

  if (issues.length === 0) {
    // No agent-workable issues — either the spec is done (the finalize check
    // below the loop decides) or what remains needs a human first.
    console.log("No unblocked issues to work on.");
    break;
  }

  console.log(
    `Planning complete. ${issues.length} issue(s) to work in parallel:`,
  );
  for (const issue of issues) {
    console.log(`  ${issue.id}: ${issue.title} → ${issue.branch}`);
    // Claim the issue (same convention wayfinder uses): assign it to the
    // authenticated account so in-flight work is visible on the board.
    sh(`gh issue edit ${issue.id} --add-assignee "@me"`);
  }

  // -------------------------------------------------------------------------
  // Phase 2: Execute + Review
  //
  // For each issue, create a sandbox via createSandbox() so the implementer
  // and reviewer share the same sandbox instance per branch. The implementer
  // runs first; if it produces commits, the reviewer runs in the same sandbox.
  //
  // Promise.allSettled means one failing pipeline doesn't cancel the others.
  // -------------------------------------------------------------------------

  const settled = await Promise.allSettled(
    issues.map(async (issue) => {
      const sandbox = await sandcastle.createSandbox({
        branch: issue.branch,
        sandbox: docker(),
        hooks,
        copyToWorktree,
      });

      try {
        // Run the implementer
        const implement = await sandbox.run({
          name: "implementer",
          maxIterations: 100,
          agent: sandcastle.claudeCode("claude-opus-4-8"),
          promptFile: "./.sandcastle/implement-prompt.md",
          promptArgs: {
            TASK_ID: issue.id,
            ISSUE_TITLE: issue.title,
            BRANCH: issue.branch,
          },
        });

        // Only review if the implementer produced commits
        if (implement.commits.length > 0) {
          const review = await sandbox.run({
            name: "reviewer",
            maxIterations: 1,
            agent: sandcastle.claudeCode("claude-fable-5"),
            promptFile: "./.sandcastle/review-prompt.md",
            promptArgs: {
              BRANCH: issue.branch,
            },
          });

          // Merge commits from both runs so the merge phase sees all of them.
          // Each sandbox.run() only returns commits from its own run.
          return {
            ...review,
            commits: [...implement.commits, ...review.commits],
          };
        }

        return implement;
      } finally {
        await sandbox.close();
      }
    }),
  );

  // Log any agents that threw (network error, sandbox crash, etc.).
  for (const [i, outcome] of settled.entries()) {
    if (outcome.status === "rejected") {
      console.error(
        `  ✗ ${issues[i]!.id} (${issues[i]!.branch}) failed: ${outcome.reason}`,
      );
    }
  }

  // Only pass branches with unmerged work to the merge phase. Gate on the
  // branch being ahead of the spec branch, not on commits made during this
  // run: a reused branch can carry commits from an earlier run (e.g. a run
  // whose merge phase failed), and those still need merging even when the
  // implementer verified the work and finished without committing anything
  // new.
  const branchAhead = (branch: string): number => {
    try {
      return Number(sh(`git rev-list --count "${specBranch}".."${branch}"`));
    } catch {
      return 0; // branch doesn't exist on the host — nothing to merge
    }
  };

  const completedIssues = settled
    .map((outcome, i) => ({ outcome, issue: issues[i]! }))
    .filter(
      (entry) =>
        entry.outcome.status === "fulfilled" &&
        branchAhead(entry.issue.branch) > 0,
    )
    .map((entry) => entry.issue);

  const completedBranches = completedIssues.map((i) => i.branch);

  console.log(
    `\nExecution complete. ${completedBranches.length} branch(es) with unmerged commits:`,
  );
  for (const branch of completedBranches) {
    console.log(`  ${branch}`);
  }

  if (completedBranches.length === 0) {
    // No branch is ahead of the spec branch — nothing to merge this cycle.
    console.log("No unmerged commits on any branch. Nothing to merge.");
    continue;
  }

  // -------------------------------------------------------------------------
  // Phase 3: Merge
  //
  // One agent merges each completed branch into the spec branch via a PR it
  // opens and immediately merges (paper trail on GitHub). Conflicts are
  // resolved locally on the sub-issue branch, pushed, then the PR is merged.
  //
  // The {{BRANCHES}} and {{ISSUES}} prompt arguments are lists that the agent
  // uses to know which branches to merge and which issues to close.
  // -------------------------------------------------------------------------
  const merge = await sandcastle.run({
    hooks,
    sandbox: docker(),
    name: "merger",
    maxIterations: 1,
    agent: sandcastle.claudeCode("claude-opus-4-8"),
    promptFile: "./.sandcastle/merge-prompt.md",
    promptArgs: {
      SPEC_BRANCH: specBranch,
      // A markdown list of branch names, one per line.
      BRANCHES: completedBranches.map((b) => `- ${b}`).join("\n"),
      // A markdown list of issue IDs and titles, one per line.
      ISSUES: completedIssues.map((i) => `- ${i.id}: ${i.title}`).join("\n"),
    },
  });

  if (!merge.completionSignal) {
    // The merger hit a blocker (auth, unresolvable conflict, …) and did not
    // finish. Replanning would just re-run the same doomed merge, so stop
    // and let a human look at the merger log.
    console.error(
      "\nMerger did not signal completion — see .sandcastle/logs/ for the blocker. Stopping.",
    );
    break;
  }

  console.log("\nBranches merged.");

  if (openSubIssues().length === 0) {
    console.log("All spec sub-issues closed. Moving to finalize.");
    break;
  }
}

// ---------------------------------------------------------------------------
// Phase 4: Finalize
//
// Only when every GitHub sub-issue of the spec is closed: open the spec PR
// against main, review it, address the review. Otherwise report what's still
// open so a human can triage (label ready-for-agent, or close) and rerun.
// ---------------------------------------------------------------------------

const remaining = openSubIssues();

if (remaining.length > 0) {
  console.log(`\nSpec #${spec.number} not finalized — open sub-issues remain:`);
  for (const issue of remaining) {
    const labels = issue.labels.map((l) => l.name).join(", ") || "no labels";
    console.log(`  #${issue.number} ${issue.title} [${labels}]`);
  }
  console.log(
    "Label them ready-for-agent (or close them) and rerun to finalize.",
  );
} else {
  console.log(`\nAll sub-issues of spec #${spec.number} closed. Finalizing.`);

  // Open the spec PR against main. fable-5: writing a human-mergeable PR
  // title/description well is a comprehension-and-judgment task.
  const specPr = await sandcastle.run({
    hooks,
    sandbox: docker(),
    name: "spec-pr",
    maxIterations: 1,
    agent: sandcastle.claudeCode("claude-fable-5"),
    promptFile: "./.sandcastle/spec-pr-prompt.md",
    promptArgs: {
      SPEC_ISSUE: String(spec.number),
      SPEC_TITLE: spec.title,
      SPEC_BRANCH: specBranch,
    },
    output: sandcastle.Output.object({ tag: "pr", schema: prSchema }),
  });
  const prNumber = String(specPr.output.number);
  console.log(`Spec PR #${prNumber} opened.`);

  // Final review: skill-first (/code-review), posted as a PR comment.
  const review = await sandcastle.run({
    hooks,
    sandbox: docker(),
    name: "final-reviewer",
    maxIterations: 1,
    agent: sandcastle.claudeCode("claude-fable-5"),
    promptFile: "./.sandcastle/final-review-prompt.md",
    promptArgs: {
      PR_NUMBER: prNumber,
      SPEC_ISSUE: String(spec.number),
      SPEC_BRANCH: specBranch,
    },
  });
  if (!review.completionSignal) {
    throw new Error(
      "Final reviewer did not signal completion — the review comment may be missing. Not running the address step; check .sandcastle/logs/.",
    );
  }
  console.log("Final review posted.");

  // Address the review in a single round; the PR then waits for a human.
  const addressed = await sandcastle.run({
    hooks,
    sandbox: docker(),
    name: "address-final-review",
    maxIterations: 30,
    agent: sandcastle.claudeCode("claude-opus-5"),
    promptFile: "./.sandcastle/address-final-review-prompt.md",
    promptArgs: { PR_NUMBER: prNumber, SPEC_BRANCH: specBranch },
  });
  if (!addressed.completionSignal) {
    console.error(
      `address-final-review did not signal completion — PR #${prNumber} may have unaddressed feedback. Check .sandcastle/logs/.`,
    );
  } else {
    console.log(
      `Review addressed. PR #${prNumber} is ready for human review and merge.`,
    );
  }
}

console.log("\nAll done.");
