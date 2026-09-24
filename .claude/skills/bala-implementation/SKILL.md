---
name: bala-implementation
description: >
  Implement a Bala story's checkpoint plan (as published by /bala-plan
  to a GitHub issue — running /bala-plan in a sub-agent first if the
  story has no plan yet), one checkpoint at a time, each done by a
  /rust-skills-following sub-agent and minimally checked before moving
  on, then closed out with a comprehensive end-to-end review, a branch,
  and a PR. Use whenever the user wants to build/implement a story
  (planned or not), work through a Bala issue's checkpoints, or turn a
  /bala-plan issue into code. Invoke with /bala-implementation <issue number or
  story number/name>.
metadata:
  internal: true
---

# Bala Implementation

You are executing a checkpoint plan that `/bala-plan` published as a
GitHub issue (or that you have it publish first, in Phase 0b, if the
story isn't planned yet), turning it into working, tested, reviewed
code on a branch with a PR — the other half of that skill's lifecycle. Work
through the phases below in order, and don't skip Phase 4 (the
end-to-end review) even when every checkpoint's own agent reported
success: individual checkpoints can each be locally correct and still
add up to a story that doesn't hang together.

---

## Phase 0 — Which issue, and starting fresh or resuming?

If the user didn't name an issue/story unambiguously, ask. Resolve it
to a concrete GitHub issue number (search by story number/title if
they gave you that instead: `gh issue list --repo <owner>/<repo>
--state all --search "Story <N.M> in:title"`).

Fetch the issue body (`gh issue view <number>` or the `github` MCP
tool) — it **is** the plan: a checkpoint task list (`- [ ] N — ...`)
plus an "out of scope" section. If there's no issue for the story
yet, or the issue exists but its body has no checkpoint task list
(a bare placeholder, or just the story text), the plan hasn't been
made — do Phase 0b before anything else. If the only match is a
*closed* issue, ask the user whether it's being reopened or whether
this is really a different story before planning over it.

If the checklist has boxes already
checked, or a comment thread shows prior partial work, this is a
**resume**: skip to the first unchecked checkpoint rather than
restarting from 0, and read whatever branch/PR already exists for this
issue first (`gh pr list --search "<issue number> in:body"`) so you
build on it instead of duplicating it.

Confirm with the user which repo/working tree you're operating in if
it isn't already obvious from context.

## Phase 0b — No plan yet? Run `/bala-plan` in a sub-agent first

Skip this phase entirely if Phase 0 found an issue with a checkpoint
list — never re-plan a story that already has one; a plan that looks
wrong is something to raise with the user, not silently regenerate.

Otherwise, the plan has to exist before anything else here makes sense
(Phase 1 reads it, Phase 3 executes it). Delegate it rather than
planning inline: `/bala-plan` reads a lot of design docs and repo state,
and keeping that out of your own context leaves room for the long
implementation run that follows.

### 0b-i. Dispatch the planning sub-agent

`/bala-plan` has two user gates a sub-agent can't pass on its own — it
can't talk to the user. Its Phase 2 asks about scope gaps, and its
Phase 3 has the user confirm the draft before anything is published.
So split the run at those gates. Spawn a `general-purpose` agent
(the `Agent` tool) whose prompt:

- States the repo path and the story (number and title, plus the
  existing issue number if Phase 0 found a planless one, so it edits
  that issue rather than creating a duplicate).
- Instructs it to invoke `/bala-plan` (the `Skill` tool, name
  `bala-plan`) and follow it, **up to but not including publishing**:
  do its Phases 0–3, and wherever the skill says to ask the user, do
  not guess — collect the question, with the skill's concrete
  recommendation, into its report instead.
- Says explicitly: do NOT create or edit the GitHub issue or
  milestone, and do NOT edit `STORIES.md` yet — those happen only after
  the user confirms.
- Asks it to report back: the full draft plan exactly as it would be
  published (the issue body, checkpoint task list plus "out of scope"),
  every scope-gap question with its recommendation, and any
  `STORIES.md` edit it would propose.

### 0b-ii. Minimal check, yourself, then the user's confirmation

Same discipline as 3b — a quick skim with a small, fixed tool budget,
not a full review:

- The draft is in the shape Phase 1 and Phase 3 will consume: a
  `- [ ]` task list of checkpoints, each naming its tests
  (`should_...`-style behavior names, tagged with ACs) and ending in a
  **Demo** line, plus an "out of scope" section.
- Every AC of the story in `docs/design/STORIES.md` is either proven by
  some checkpoint's tests or listed out of scope — nothing silently
  dropped.
- Spot-check two or three types/methods/error variants the checkpoints
  cite against the LLDs or the actual code (`grep`), to catch an
  invented signature.
- It hasn't re-planned something earlier stories already built (a crate
  or schema that already exists in the repo).
- Nothing was published or edited: no new/changed issue, and
  `git status` is as clean as it was before.

If something's off, send the same agent a follow-up (`SendMessage`)
rather than fixing the draft yourself. Once it passes, bring the
agent's scope-gap questions and the draft to the user — the
confirmation `/bala-plan` requires still belongs to them even though a
sub-agent wrote the draft; a plan nobody signed off on is exactly the
kind of thing that surfaces as a Phase 4 judgment call much later and
more expensively. If the user's answers change the draft, relay them to
the same agent and re-check the revision.

### 0b-iii. Publish, and verify it landed

On the user's confirmation, `SendMessage` the same agent to finish
`/bala-plan`'s Phase 4 (milestone, issue, any confirmed `STORIES.md`
edit) and report the issue number/link. Then confirm it yourself:
`gh issue view <number>` shows the confirmed body with its task list,
and the issue is in the story's `Epic N: <Title>` milestone. That issue
is now the plan — carry on to Phase 1 with it.

A `STORIES.md` edit from this phase is expected, uncommitted work in
the tree: it's part of this story, so Phase 1's clean-tree check should
treat it as such, and it goes into the story's commit in Phase 5.

## Phase 1 — Load context once, up front

Before touching any checkpoint, read (once, into your own context —
this is what every sub-agent you spawn will need restated, since each
starts cold):

1. The issue body itself — every checkpoint, in order, plus the "out
   of scope" list.
2. `docs/design/STORIES.md` — the target story's full text, for the
   acceptance criteria the checkpoints are proving.
3. `docs/design/HLD.md` and whichever `docs/design/<component>.md` LLDs
   the story's checkpoints touch — the real types, signatures, error
   variants, schema, and testing conventions checkpoints must build
   against. You'll be citing these in every sub-agent prompt so it
   doesn't invent a signature the LLD already settled.
4. Current repo state: `find`/`ls` the crates that exist, what's
   already implemented vs. still a placeholder, `git status` (is the
   tree clean?), `git log --oneline -10`. If mid-story work already
   exists in the tree from a prior session, treat it like a completed
   checkpoint's output — verify it (Phase 3) before building on it,
   don't redo it.

If the working tree isn't clean — beyond a `STORIES.md` edit Phase 0b
just made — and this isn't a deliberate resume, stop and ask the user how to proceed rather than mixing unrelated
changes into this story's commit.

## Phase 2 — Branch

Create (or check out, if resuming) a feature branch off the repo's
default branch before any checkpoint work starts:

```
git checkout -b story-<N.M>-<short-slug>
```

Never implement checkpoints directly on the default branch.

## Phase 3 — Execute checkpoints, one at a time, in order

**Do not run checkpoints in parallel** — later checkpoints build on
earlier ones' actual code, not just the plan's description of it, so
each sub-agent needs the real current state of the repo, which only
exists once the prior checkpoint has landed.

For each checkpoint, in plan order:

### 3a. Dispatch one sub-agent per checkpoint

Spawn a `general-purpose` agent (the `Agent` tool) with a prompt that:

- States the repo path and exactly one checkpoint's plan text (quoted
  from the issue), not the whole plan — a sub-agent should not
  freelance ahead into later checkpoints.
- Summarizes current repo state relevant to this checkpoint: which
  files/modules exist already, what they currently contain (types,
  traits, test counts), so the agent doesn't have to rediscover it and
  doesn't accidentally redo Phase 1's reading from scratch.
- Cites the specific LLD section(s) governing this checkpoint's types,
  signatures, schema, or error variants — tell it to read the LLD
  itself for the full picture, but pin the parts it must not deviate
  from without asking.
- Explicitly scopes which crates/files it may touch and which it must
  leave alone (e.g. "do not touch bala-store in this task; if you find
  you need a change there, stop and report back instead of editing
  it") — cross-checkpoint scope creep is the most common way a
  sub-agent's local decision breaks a later checkpoint's assumptions.
- Instructs it to invoke `/rust-skills` (the `Skill` tool, name
  `rust-skills`) before writing code, and follow it throughout.
- Requires strict TDD: write the named tests first (name them
  explicitly if the plan does), confirm red, then implement to green,
  then refactor — not implement-then-backfill-tests.
- Tells it to run the relevant `cargo test`/`cargo clippy --all-targets`/
  `cargo fmt --check` (scoped to the crate(s) touched, plus a
  workspace-wide build/test to catch collateral breakage) before
  reporting done.
- Says explicitly: do NOT commit or push; leave the working tree for
  review.
- Asks it to report back: files created/modified, every test name
  written and confirmation of red→green, any deliberate design
  decisions or deviations it made (and why), and confirmation
  build/test/clippy/fmt all pass.

### 3b. Minimal check, yourself, before moving on

When the sub-agent reports back, don't just trust the summary — spend
a small, fixed amount of your own tool budget confirming the plan step
was actually followed:

- Re-run the test/clippy/fmt commands yourself (cheap, and the ground
  truth — a sub-agent's self-report can be stale or wrong).
- Skim the new/changed files for the specific things this checkpoint's
  plan text asked for (the named tests exist, the named types/methods
  exist with roughly the right shape) — a skim, not a full code review;
  the deep review is Phase 4.
- Confirm no unrelated crate was touched, and no stray scratch/backup
  files were left behind.
- Treat any inline tool diagnostic that contradicts a command you just
  ran cleanly (e.g. a stale-looking "unresolved import" after `cargo
  build` already succeeded) as a stale editor/LSP snapshot, not a real
  failure — verify by re-running the actual command before trusting
  either signal.

If something's off, don't silently fix it yourself and don't silently
accept it — either send the same sub-agent a follow-up (if it's still
addressable, e.g. via `SendMessage` to continue it) or spawn a small
corrective task, and only proceed once this checkpoint is actually
right. If the deviation looks like it might legitimately need a scope
change to the plan itself (not just a bug), pause and ask the user
rather than deciding unilaterally.

### 3c. Move to the next checkpoint

Only after a checkpoint passes its minimal check. Repeat 3a–3b for
every remaining checkpoint in order.

## Phase 4 — Comprehensive end-to-end review

Once every checkpoint is done, step back and review the *story*, not
just the sum of checkpoints: a fresh sub-agent for the review itself
(same reasoning as Phase 3 — it needs to read the real, current state
of every file the story touched, not a description of it), then a
minimal check of that agent's report before deciding the story is
done.

### 4a. Dispatch one review sub-agent for the whole story

Spawn a `general-purpose` agent with a prompt that gives it everything
Phase 1 gave you — it's reviewing the whole story, not one checkpoint,
so it needs the full picture a checkpoint sub-agent deliberately
didn't get:

- The repo path, the branch name, and the full issue body (every
  checkpoint plus the "out of scope" list) verbatim.
- The target story's full text from `docs/design/STORIES.md`.
- Which `docs/design/HLD.md`/`docs/design/<component>.md` LLD sections
  govern this story, so it can check the merged code against them
  itself rather than trusting each checkpoint sub-agent's own claim of
  compliance.
- Instructions to find the actual diff itself (e.g. `git diff
  <default-branch>...HEAD` or `git log` since the branch point) rather
  than being handed a description of what changed.

Ask it to work through, and report on, each of:

1. Run the full workspace suite: `cargo test --workspace`, `cargo
   clippy --workspace --all-targets -- -D warnings`, `cargo fmt
   --check`. All must be clean.
2. Read through the actual production code changed across all
   checkpoints (not just each checkpoint's own diff in isolation) —
   check that later checkpoints didn't quietly contradict an earlier
   one's design choice, that error handling is consistent across the
   whole slice, and that naming/module layout reads as one coherent
   piece of work rather than several disconnected patches.
3. Re-verify every acceptance criterion the issue's story claims to
   cover actually has a passing test or an honest, documented reason it
   doesn't (deferred to a later story, per the issue's own "out of
   scope" section).
4. Re-run the plan's own "Demo" line(s), manually, for at least the
   final checkpoint (and any earlier ones worth spot-checking) — actual
   commands, actual output, not a restatement of what a checkpoint's
   own demo claimed.
5. Check every deliberate deviation any checkpoint's own work
   surfaced (a scope cut, a placeholder error type, a no-op seam for a
   future story) against the plan's "out of scope" list — flag any
   that's a silent narrowing rather than something the plan already
   sanctioned.
6. **Adversarial correctness/efficiency pass.** A green test suite only
   proves the plan's own named tests pass — it says nothing about bug
   classes nobody wrote a test for yet. Re-read every changed method
   the way a reviewer looking for trouble would, not the way the author
   who just made it pass would. Ask, for each one:
   - **Could two of its `Store` calls interleave with someone else's
     write?** Any method that reads state, decides something from it,
     and then writes needs that whole sequence atomic — one
     transaction, not several with a gap in between.
   - **Is it doing the same work twice?** Look for a second fetch,
     scan, or query that a first one's result already contained, or a
     loop re-deriving something computed just above it.
   - **Do all implementations of the trait it calls actually agree?**
     When more than one type implements the same `Store`/`StoreTx`
     method (the real backend and the in-memory test fake, most
     often), a subtle semantic a real caller relies on — idempotence,
     ordering, uniqueness — is only as good as the *least* correct
     implementation of it.
   - **Is any loop or recursion bounded by something other than the
     data's actual size?** Recursion or iteration whose depth tracks a
     user-built structure (a parent chain, a dependency graph) needs a
     bound that scales with the data, not the call stack or a fixed
     buffer.
   - **What happens if this is called again with no real change to
     make?** A mutation that could be invoked on something already in
     its target state should be a true no-op — no timestamp bump, no
     spurious entry in a "touched"/"changed" result, no overwriting a
     value that was already correct.
   - **Did this change leave a sibling description behind?** A new
     case, variant, or subcommand should show up everywhere its
     siblings are enumerated — doc comments, help text, match arms —
     not just where the compiler forces it to.

   This list names the shapes of bug that have actually slipped through
   this process before (a multi-transaction race, a redundant tree
   fetch, a test-fake/real-backend divergence, unbounded recursion, a
   non-idempotent completion, stale help text) as concrete illustrations
   of each question — treat them as examples of what to look for, not
   the complete set of what to check. If something doesn't fit any
   bullet above but still smells like the code would surprise its own
   author under a slightly different input, trust that instinct and dig
   in rather than waiting for it to match a named category.

For anything it's confident is a genuine, narrowly-scoped bug (matching
the shapes in step 6, or an outright contradiction between two
checkpoints), tell it to fix it directly — write the regression test
first, then the fix, then re-run the full suite — the same discipline
a checkpoint sub-agent follows. For anything that's a judgment call
about design intent (a real behavioral discrepancy between what two
checkpoints assumed, a plan step that turned out to be wrong once
implemented, an "out of scope" item that no longer looks right), it
must not decide unilaterally — it should leave it unfixed and flag it
clearly in its report instead.

Tell it explicitly: do NOT commit or push. Ask it to report back: the
full suite's pass/fail, every acceptance criterion's status, the
demo's actual transcript, every fix it made (file, what, why, the
regression test's name), and every judgment-call item it left flagged
rather than deciding.

### 4b. Minimal check, yourself, before moving on

Same discipline as 3b, scaled to the whole story: don't take the
review agent's report on faith just because it was thorough.

- Re-run `cargo test --workspace`, `cargo clippy --workspace
  --all-targets -- -D warnings`, and `cargo fmt --check` yourself.
- Spot-check at least one of its fixes against the diff, and re-run the
  demo command(s) yourself rather than trusting its transcript alone.
- Read its list of flagged judgment calls; for each, decide whether
  it's actually a story-blocking issue or a documented, in-scope
  deferral it simply flagged out of caution.

If something's off, don't silently fix it yourself and don't silently
accept it — send the same review agent a follow-up (`SendMessage`) or
spawn a small corrective task, and only move to Phase 5 once the story
is actually right.

**Stop and ask the user** for anything the review agent flagged as a
judgment call that you can't confidently rule on yourself either: a
real behavioral discrepancy between what two checkpoints assumed, a
plan step that turned out to be wrong once implemented, or anything
that would require a design decision rather than a mechanical fix.
Don't paper over it to reach a green build.

## Phase 5 — Commit, push, PR

Once Phase 4 is clean:

1. `git add` exactly the files this story's checkpoints produced (sanity
   check `git status` first — nothing unrelated should be staged).
2. One commit (or, if the user prefers a commit per checkpoint, ask
   before assuming — default to one commit for the whole story, since
   checkpoints aren't meaningful standalone history once the story is
   done) summarizing what was built, checkpoint by checkpoint, test
   counts, and any deliberate scope deviations — end with this
   session's attribution footer.
3. Push the branch, then open a PR (check for a
   `pull_request_template.md`/`.github/PULL_REQUEST_TEMPLATE/` first
   and use it if present) with:
   - `Closes #<issue>`.
   - The checkpoint list (as a recap, checked off).
   - Verification summary (test counts, clippy/fmt clean).
   - Acceptance criteria covered.
   - Deliberate scope cuts, so a reviewer sees them as decisions on
     record rather than discovering them in the diff.
   - This session's attribution footer.

## Phase 6 — Hand back

Report the branch and PR link. Note which checkpoints (if any) needed
a correction during Phase 3b, and summarize anything flagged in Phase
4 that the user should be aware of even though it didn't block
merging.

---

## Style notes

- Sub-agent prompts should be self-contained enough that a cold agent
  with no memory of this conversation can execute correctly — restate
  the relevant repo state and LLD constraints every time, don't assume
  it can infer them.
- Favor one sub-agent per checkpoint over one sub-agent for several
  checkpoints, even when a checkpoint looks small — the minimal-check
  gate between checkpoints is what catches drift early, and it only
  works if checkpoints are checked individually.
- Never let a sub-agent decide, on its own, to touch a crate outside
  its assigned checkpoint's scope, extend a trait beyond what the
  current checkpoint needs, or "fix" an earlier checkpoint's code —
  that's how a plan's careful sequencing silently unravels. Any such
  need is a signal to stop and report, not to route around.
- The comprehensive review (Phase 4) is not optional busywork — it's
  the step that catches what individual-checkpoint testing structurally
  cannot: two checkpoints that are each internally consistent but
  disagree with each other, and the bug shapes its own step 6 asks
  about, which a plan's own named tests don't set out to catch because
  the plan was written before the code existed to have them.
- Phase 4's review agent gets the whole story's diff and every
  checkpoint's context at once — unlike a checkpoint sub-agent, which
  is deliberately scoped narrow, the review agent's entire job is
  seeing across checkpoints, so don't trim what you hand it the way you
  would for Phase 3a.
- The review agent may fix a genuine, narrowly-scoped bug itself
  (regression test first, same as a checkpoint sub-agent), but must
  never resolve a design judgment call on its own — that distinction is
  the same "fix vs. flag" line 3a's sub-agents already draw for
  cross-checkpoint scope creep, just applied to the whole story instead
  of one checkpoint's boundaries.
- If the user says "stop if you can't rule on yourself," take that
  literally in Phase 4: a genuine judgment call about design intent —
  whether it surfaces from the review agent's report or from your own
  4b check — goes back to the user, not into a code comment justifying
  a guess.
