---
name: code-review
description: >
  Conduct multi-axis code review on pull request diffs. Check correctness,
  malicious code, security boundaries, breaking API changes, and system fit.
---

# code review

Conduct multi-axis code review on PR diff across six review axes.

## rules

- standard: approve when change improves overall code health.
- no linter duplication: never check formatting, indentation, whitespace, semicolons, or import order. Linters and CI handle formatting deterministically.
- no duplicate tooling: automated tests and typechecks verify build. Inspect logic, contracts, edge cases.
- malicious code guard: verify diff contains no backdoors, data exfiltration, obfuscated logic, dangerous eval, credential theft, or environment tampering.
- diff only: review provided diff; ignore excluded build outputs, lockfiles, migrations, and binary assets.

## six review axes

### 1. correctness
- match spec and requirements.
- handle boundary values, null, undefined, empty state.
- handle error paths and unhandled exceptions.
- tests cover behavior and edge cases, not just happy path.
- check off-by-one errors, race conditions, unhandled async rejection.

### 2. readability and simplicity
- clear naming, direct control flow, no deep callback nests.
- abstractions earn complexity; no premature generalization.
- dead code check: remove orphaned helpers, unused variables, stale shims.
- no bolted-on conditionals across unrelated flows.

### 3. architecture
- match repo tier conventions and boundaries.
- clean module separation; dependencies flow in correct direction.
- no feature-specific logic inside shared modules.
- explicit type boundaries; avoid gratuitous casts, any, or silent fallbacks.

### 4. security and safety
- verify code not malicious. No hidden network requests, eval, data exfiltration, or backdoor scripts.
- validate and sanitize input at trust boundaries.
- keep secrets out of code, commits, and logs.
- parameterize database queries. Prevent injection.
- treat external data as untrusted.

### 5. performance
- no n+1 queries or unindexed scans.
- avoid unbounded loops or unbounded data fetches.
- avoid blocking sync calls in async hot paths.
- avoid unnecessary re-renders in UI components.

### 6. workflows and documentation
- workflows (.github/workflows): verify least privilege permissions, secure triggers, no secret exposure on untrusted PRs, valid YAML syntax.
- agent guides and markdown (.agents/, *.md): plain text only, no bold text, no emojis, factual accuracy, clear instructions.

## structural remedies

Suggest concrete restructuring when flagging issues:
- collapse duplicate branches into single clearer flow.
- separate orchestration from business logic.
- move feature logic out of shared packages.
- reuse canonical helpers instead of bespoke duplicates.
- delete pass-through wrappers that add indirection without value.

## review format

every review must follow this exact order:

### 1. tldr (/caveman mode)
write top-level summary in /caveman mode (1-3 sentences).
drop articles (a/an/the), drop filler, keep technical facts exact. Plain text only, no bold asterisks, no emojis.
pattern: `[thing] [action] [reason]. [status/verdict].`
example: `tldr: diff update ai review action, switch model to nemotron, remove dead test code. security sound, logic clean.`

### 2. findings
format findings: `<file>:L<line>: <sev>: <problem>. <fix>.`

severity levels:
- critical: blocks merge. Malicious code, confirmed exploitable vulnerability, data loss, secret leakage.
- required: blocks merge. Concrete, reproducible runtime bug, regression, or broken error handling.
  Must NOT be used for:
  * Platform environment design (e.g. standard desktop user profile directory permissions vs custom Win32 DACLs).
  * Defensive-in-depth suggestions where data is already sanitized upstream.
  * Questioning compile-time guarantees (e.g. Send/Sync in Rust) that CI compiler already verifies.
  * Demanding secret/password hashes in cache keys (scoping cache by username is the standard pattern; credentials are kept in OS keychain).
- optional: suggestion. Non-blocking improvement, defense-in-depth suggestion, or pattern optimization.
- nit: minor cleanup, documentation polish, or cosmetic feedback.

if no defects found, output: `no critical or required issues found.`

### 3. verdict
deliver clear verdict at end of review on its own line:
verdict: APPROVE
or
verdict: REQUEST CHANGES
