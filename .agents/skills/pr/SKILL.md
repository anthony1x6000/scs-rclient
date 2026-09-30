---
name: pr
description: Scope-check changes, commit, push branch, and open one single-responsibility PR with gh.
---

# /pr

Worktree isolation is mandatory. Never run /pr from the main checkout. All commit, push, and `gh` steps run inside a linked worktree only.

One PR carries one responsibility. A PR is the smallest complete change that answers a single "why": if its body needs an "also" or a "while I was here" clause, the change set is too broad and must be split before the PR is opened.

Batch the following operations into a single bash execution wherever possible:

0. **Worktree gate (mandatory, do first)**: from the main checkout top-level, verify where you are before touching anything:
   ```bash
   git rev-parse --show-toplevel
   git worktree list
   git status --short --branch
   ```
   - if cwd is the main checkout (toplevel owns `.worktrees/`), stop. Do not commit, push, or open a PR from here. Create one worktree per task, then re-run /pr inside it:
     ```bash
     git fetch origin staging
     git worktree add .worktrees/<slug> -b <type>/<slug> origin/staging
     cd .worktrees/<slug>
     ```
     `<slug>` is short kebab-case for the task. `<type>` is `feat`, `fix`, `docs`, `chore`, or `refac` per commit protocol. Verify with `git worktree list`.
   - if the main checkout is dirty, carry over only the changes that belong to this task, do not commit them there:
     ```bash
     git stash push -u -m "pr-wip-<slug>" -- <task paths>
     git fetch origin staging
     git worktree add .worktrees/<slug> -b <type>/<slug> origin/staging
     cd .worktrees/<slug> && git stash pop
     ```
     Dirty files that are not this task's are someone else's work in progress: leave them untouched in the main checkout, never stash them, never revert them, and branch from a clean `origin/staging`.
   - if cwd is already inside `.worktrees/<slug>`, continue. Never `cd` back to the main checkout mid-flow. Main checkout stays clean.
   - see `.agents/rules/operations.md` for the full worktree isolation protocol.

1. **Preflight (inside worktree only)**: Verify `gh auth status`. Ensure base branch is `staging` (or user override via `--base`). Never switch branches in the main checkout; if the worktree branch is `staging` or `main`, branch off to `<type>/<name>` first.
2. **Browser walkthrough (opt-in only)**: never author `.agents/walkthrough.ts` or add a `## browser walkthrough` section by default. Do so only when you judge a walkthrough valuable (visual UI changes, routes, flows where visual proof aids review) or when the user explicitly requested `/pr-walkthrough` or a browser walkthrough. If the base branch contains a stale `.agents/walkthrough.ts` and neither condition holds, delete it from your branch so CI skips the recording.
3. **Scope gate (mandatory, before staging anything)**: a PR must not carry more responsibility than one task. Inspect the full change set and cut it down before it becomes a PR:
   ```bash
   git status --short
   git diff --stat HEAD
   git log --oneline origin/staging..HEAD
   ```
   - **one responsibility**: describe the PR in a single sentence of the form "this changes X so that Y". If that sentence needs "and", or an "also"/"while I was here" clause, the branch holds more than one responsibility: split it into separate worktree/branch/PRs, or drop the extras.
   - **no passengers**: unrelated refactors, formatting sweeps, dependency bumps, dead-code deletion, doc edits, and opportunistic bug fixes do not ride along even when they are small, correct, and already written. Move them to their own branch, or revert them (`git restore <path>`, `git stash push -u -- <path>` for later).
   - **blast radius**: every changed file must be one the task cannot be correct without, meaning the implementation, its tests, its types, its docs, or its required config. Files that are merely nearby are out.
   - **outsized diff is a defect**: if the diff spans unrelated subsystems, mixes a feature with a refactor, or is too large to read in one sitting, split it. Smaller single-purpose PRs review faster and revert cleanly.
   - **stack instead of mixing**: when the parts genuinely cannot land separately, stack the PRs (base each on the previous branch) and state the stack and merge order in the body. Never merge two responsibilities into one PR to avoid the stack.
   - **do not widen to look complete**: a small PR that fully solves one problem beats a large PR that half-solves several.
4. **Commit (inside worktree only)**: Stage only the files that survived the scope gate. If uncommitted changes exist, commit using conventional commits (`feat:`, `fix:`, `docs:`, `chore:`, `refac:`, <=72 chars) with trailer `Co-authored-by: <model-name> <bot@anthonyis.online>` (using the agent's actual model name). The subject must name the PR's single responsibility, not a list.
5. **Push & PR (inside worktree only)**:
   ```bash
   BRANCH=$(git branch --show-current)
   git push -u origin "$BRANCH"
   PR_URL=$(gh pr create --base staging --head "$BRANCH" --title "<summary>" --body "<body>")
   ```
   The title and body state the one responsibility and why it exists; if they cannot, return to the scope gate. Never run `gh pr merge --auto` here. GitHub auto-merge only waits for required status checks (`verify`) and prematurely merges before other CI workflows (per-app CI, walkthrough, audio-smoke) finish or fail.
6. **Watch Cloud Checks**: Sleep 60s after creating the PR (checks need time to appear; `--watch` exits early on "no checks"). Then one blocking call: `gh pr checks "$PR_URL" --watch --fail-fast --interval 30`. If any checks fail, inspect failure (`gh run view <run-id> --log-failed`) and diagnose. Never merge on red CI.
7. **Merge (only after all checks pass)**:
   ```bash
   gh pr merge "$BRANCH" --squash
   ```
8. **Cleanup**: after the PR merges, remove the task worktree from the main checkout top-level: `git worktree remove .worktrees/<slug>`. Use `--force` only for an intentionally discarded branch.
