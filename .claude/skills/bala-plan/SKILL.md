---
name: bala-plan
description: >
  Create or revise a strict-TDD implementation plan for one Bala user
  story from STORIES.md, and publish it as a GitHub issue (in that
  story's Epic milestone) with the plan as a checkpoint checklist. Use
  whenever the user wants to plan how to build a story, break a story
  into TDD steps, or turn a story into trackable GitHub work. Invoke
  with /bala-plan <story number or name>.
metadata:
  internal: true
---

# Bala Plan

You are turning one user story from `docs/design/STORIES.md` into a
strict-TDD implementation plan, then publishing it as a GitHub issue.
This output is transient by nature — it lives as long as the story takes
to ship, then the issue closes. That's why it belongs in GitHub, not
`docs/design/`: a milestone per Epic, an issue per Story, the plan as
that issue's checklist body. There is no committed plan file.

Work through the phases below in order.

---

## Phase 0 — Which story, and new plan or revision?

If the user didn't name a story unambiguously, ask which one (a story
number like "1.1", or close enough text to find it in STORIES.md
unambiguously).

Then check GitHub for an existing plan issue before drafting anything:

```
gh issue list --repo <owner>/<repo> --state all --search "Story <N.M> in:title"
```

- **Found, open** → this is a **revision**. Read its current body
  (`gh issue view <number>`) and the reason for revising (new scope
  learned during implementation, a checkpoint needs splitting, an AC
  turned out to need a design decision this skill's Phase 2 should have
  caught) before editing.
- **Found, closed** → confirm with the user whether they want it reopened
  (more work discovered) or whether this is really a *new*, later story
  that happens to share a title fragment — don't silently reuse it.
- **Not found** → new plan, continue to Phase 1.

## Phase 1 — Read the story in context

Read, in this order:

1. `docs/design/STORIES.md` — the target story's full text (description +
   every AC), plus the rest of its Epic for surrounding context and the
   file's own stated build order (currently: Epic 1 → 2 → 3 → 4).
2. `docs/design/HLD.md` — which component(s) (Core Library, Data Store,
   CLI/TUI Client, ...) this story's Epic belongs to, per its **Component
   responsibilities** table.
3. Every `docs/design/<component>.md` LLD for those components — the
   actual types, method signatures, error variants, schema, and testing
   conventions (`should_...` names) this plan's checkpoints must produce
   real tests and code against. Don't invent a signature — cite the LLD's.
4. **Actual repo state**, not just design docs: `find`/`ls` the crates
   that exist already (`Cargo.toml`, `src/` layout), and skim
   `git log --oneline` for prior story issues/PRs. A later story in an
   Epic is not greenfield — don't re-propose scaffolding a crate, schema,
   or trait that an earlier story's plan already built. Only plan the
   delta this story actually adds.

## Phase 2 — Find and resolve scope gaps

This is the phase most likely to be skipped and shouldn't be. A story's
ACs are product-level and often outrun what the LLDs have actually
pinned down — e.g. Story 1.1 originally listed an `assignee` field with
no `User` entity or store designed anywhere. Look for:

- A field/behavior an AC names that has no corresponding type, method, or
  schema column in any LLD yet.
- An AC that depends on a capability a *later* story owns (e.g. an early
  story implying blocked-status or rollup display before Epic 3/2 build
  it) — these are fine to note as "proven at the library layer only" but
  should not silently pull the later story's whole feature forward.
- Ambiguity in how much of the vertical slice (Core Library only vs. also
  Data Store vs. also a minimal CLI surface) this story should stand up,
  especially for the first story to touch a given component.

For each gap found, **ask the user** (don't guess silently) with a
concrete recommendation, the way you would for any underspecified
requirement. If the answer is "defer this field/AC to later," propose —
and on confirmation, apply — an edit to `STORIES.md` that makes the
deferral explicit rather than just dropping it from your plan: either a
new placeholder story (see Story 1.1a in STORIES.md for the pattern: a
short Description, ACs stubbed as open questions, `**Status:** Not yet
planned`) or a note under the affected AC explaining what moved where and
why. A plan that quietly narrows a story's scope without updating the
story text is a plan that lies about what "done" means.

## Phase 3 — Draft the checkpoint plan

Sequence the work as a list of **checkpoints**, each one a complete
red→green→refactor slice that ends in something runnable or demoable —
never a checkpoint that's "half a feature, nothing to run yet." Favor
more, smaller checkpoints over fewer large ones; the point is the user
can stop after any checkpoint, run something, and course-correct.

For each checkpoint:

- State what layer/crate it touches and why it's sequenced where it is
  (e.g. store-backed tests can't run before the schema exists; a CLI
  subcommand can't be demoed before the library method it calls exists).
- List the specific tests to write **first**, named for behavior per this
  repo's convention (`create_task_should_reject_empty_title`, not
  `test_create_task_1`), each tagged with the AC it proves where one
  applies.
- Say what minimal production code makes each test pass — cite real
  types/methods from the LLDs (§Phase 1.3), not placeholders.
- End with a one-line **Demo**: the concrete command or test run that
  proves the checkpoint works (`cargo test -p bala-core`, or a `cargo
  run -- ...` invocation once a CLI checkpoint exists).

Close the plan with an explicit **out of scope** list: anything a reader
might reasonably expect this story to cover that it deliberately doesn't
(deferred fields per Phase 2, later-Epic capabilities, unrelated
polish) — so scope is a decision on record, not a gap someone finds
later.

Present this draft to the user before publishing anything to GitHub —
confirm the checkpoint slicing and any Phase 2 scope calls before they're
written into an issue and (if applicable) STORIES.md.

## Phase 4 — Publish to GitHub

Once the user confirms the draft:

1. **Milestone.** Find the story's Epic's milestone (`gh api
   repos/<owner>/<repo>/milestones`, matching `Epic <N>: <Title>` against
   STORIES.md's own `## Epic N: <Title>` heading). Create it if absent:
   ```
   gh api repos/<owner>/<repo>/milestones -f title="Epic N: <Title>" -f description="<one line citing the Epic's stories and STORIES.md>"
   ```
2. **Issue.** Title `Story <N.M> — <Story Title>` (em dash, matching
   STORIES.md's own story heading style). Body is the checkpoint plan
   from Phase 3, as a markdown task list (`- [ ]` per checkpoint), plus
   the "out of scope" section. New plan → `gh issue create --milestone
   "Epic N: <Title>" --body-file <path>`; revision → `gh issue edit
   <number> --body-file <path>` (or comment, if the user wants history of
   what changed rather than a silent rewrite — ask which for a
   substantial revision).
3. If Phase 2 produced a `STORIES.md` edit the user confirmed, make sure
   it's applied before or alongside publishing the issue, not after —
   the issue's ACs should match the story file at the moment the issue is
   created.
4. End every generated body with the standard attribution footer used
   elsewhere in this session (git commit / PR trailer convention) if the
   surrounding session has one active.

## Phase 5 — Hand back

Report the milestone and issue links, and note the convention for
closing the loop later: PRs that implement a checkpoint should reference
`Closes #<issue>` (or check the box manually) so the issue's progress bar
and eventual auto-close track real work, not just the plan.

---

## Style notes

- Match STORIES.md's own voice for anything you write back into it:
  terse, AC-numbered, no filler.
- Checkpoints are for a human pairing with an AI assistant to execute
  next, in order — write them as instructions to *do*, not as a status
  report of work already imagined done.
- Never invent a type, method, error variant, or schema column that
  isn't in an LLD or an earlier story's actual shipped code — if the plan
  needs one that doesn't exist yet, that's a Phase 2 gap to raise, not a
  detail to quietly assume.
- Keep the whole plan proportional to the story: a small story (a single
  CRUD method) does not need nine checkpoints, and a story spanning two
  components does not fit in two.
