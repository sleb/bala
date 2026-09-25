---
name: bala-plan
description: >
  Create or revise a strict-TDD implementation plan for one Bala user
  story (a GitHub issue labeled `story`), and write it into that issue as
  a checkpoint checklist, with the story in an epic milestone. Use
  whenever the user wants to plan how to build a story, break a story
  into TDD steps, or turn a story into trackable GitHub work. Invoke
  with /bala-plan <issue number or story title>.
metadata:
  internal: true
---

# Bala Plan

You are turning one user story into a strict-TDD implementation plan and
writing that plan into the story's own GitHub issue. Stories live only in
GitHub. There is no stories file in the repo:

- **A story** is an issue labeled `story`. Its body holds the
  description, acceptance criteria (ACs), and (once planned) the plan,
  followed by a `## Changelog`.
- **An epic** is a milestone (`Epic N: <outcome>`). Its description
  holds a goal, exit criteria, a fixed story list, and a `Scope changes:`
  log.
- **The backlog** is every open issue with no milestone.
- **Areas** are `area:core` / `area:store` / `area:cli` / `area:tui`
  labels.

The plan is transient: it lives as long as the story takes to ship.
That's why it belongs in the issue, not in `docs/design/`.

Work through the phases below in order.

---

## Phase 0 — Which story, and new plan or revision?

If the user didn't name a story unambiguously, ask which one (an issue
number, or a title close enough to find it unambiguously with `gh issue
list --label story --state all --search "<words> in:title"`). Older
stories are titled `Story N.M — <Title>`, so a user may name one by
that number. Search for it the same way.

If the user describes a capability that has **no story issue yet**,
draft one (Description as "As a …, I want …, so that …", numbered ACs,
the Changelog section from §Changelog below), confirm it with the user,
and create it with `--label story` plus the matching `area:*` label(s)
and no milestone. It starts in the backlog. Then continue.

With the issue in hand (`gh issue view <number>`):

- **Open, no `## Plan` section** → new plan. Continue to Phase 1.
- **Open, has a `## Plan`** → this is a **revision**. Read the current
  plan and the reason for revising (new scope learned during
  implementation, a checkpoint needs splitting, an AC turned out to need
  a design decision Phase 2 should have caught) before editing.
- **Closed** → confirm with the user whether they want it reopened
  (more work discovered) or whether this is really a *new* story that
  should get its own issue — don't silently reuse it.

**Milestone check.** A story in the backlog (no milestone) must be
scheduled into an epic before it's planned: planning is the moment work
gets committed to. Ask the user which epic, recommending one whose goal
the story serves.

- **Adding to an existing open epic changes its scope.** It needs the
  user's explicit OK, and a dated line appended to that milestone's
  `Scope changes:` log (what was added and why).
- **A new epic needs a goal and exit criteria.** Draft them for the
  user to confirm: one or two concrete, checkable conditions, like a
  checkpoint's Demo line. Name the epic after its outcome ("Dependencies
  block and reschedule work"), not an area ("Dependencies").

## Phase 1 — Read the story in context

Read, in this order:

1. The story issue — description and every AC. Also skim the other
   stories in its milestone (`gh issue list --milestone "<title>"
   --state all`) and the milestone's goal and exit criteria, for
   surrounding context. The overall build order is in `docs/design/HLD.md`
   §Build Order and Tracking.
2. `docs/design/HLD.md` — which component(s) (Core Library, Data Store,
   CLI/TUI Client, ...) this story touches, per its **Component
   responsibilities** table.
3. Every `docs/design/<component>.md` LLD for those components — the
   actual types, method signatures, error variants, schema, and testing
   conventions (`should_...` names) this plan's checkpoints must produce
   real tests and code against. Don't invent a signature — cite the LLD's.
4. **Actual repo state**, not just design docs: `find`/`ls` the crates
   that exist already (`Cargo.toml`, `src/` layout), and skim
   `git log --oneline` for prior story PRs. A later story is not
   greenfield — don't re-propose scaffolding a crate, schema, or trait
   that an earlier story already built. Only plan the delta this story
   actually adds.
5. **User-facing docs** (`docs/*.md` outside `docs/design/` — e.g.
   `docs/cli.md`, `docs/config.md`): skim whichever ones describe the
   surface this story touches (a command, flag, keybinding, config
   format, file layout). These describe *shipped* behavior, not design
   intent, so a story that adds or changes any of it has a doc update due
   alongside the code, not as an afterthought — see Phase 3.

## Phase 2 — Find and resolve scope gaps

This is the phase most likely to be skipped and shouldn't be. A story's
ACs are product-level and often outrun what the LLDs have actually
pinned down — e.g. the first "create a task" story originally listed an
`assignee` field with no `User` entity or store designed anywhere. Look
for:

- A field/behavior an AC names that has no corresponding type, method, or
  schema column in any LLD yet.
- An AC that depends on a capability another, unbuilt story owns (e.g.
  an early story implying blocked-status display before dependencies
  exist) — fine to note as "proven at the library layer only", but don't
  silently pull the other story's whole feature forward.
- Ambiguity in how much of the vertical slice (Core Library only vs. also
  Data Store vs. also a minimal CLI surface) this story should stand up,
  especially for the first story to touch a given component.

For each gap found, **ask the user** (don't guess silently) with a
concrete recommendation, the way you would for any underspecified
requirement. If the answer is "defer this field/AC to later," make the
deferral explicit rather than just dropping it from your plan:

- File a **backlog issue** for the deferred part (label `story` if it's
  user-facing, plus `area:*`; no milestone). Its body opens with
  `**Origin:** follow-up to #<story> AC<n> — <why>`. #68 shows the
  pattern.
- Edit the story's affected AC to say what moved where (`… (deferred to
  #<new>)`), and log it in the story's Changelog.

A plan that quietly narrows a story's scope without recording it is a
plan that lies about what "done" means.

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
  applies. The AC tags are for the plan only. Don't tell the implementer
  to put issue, AC, or checkpoint numbers in test names or code
  comments, and don't phrase planned comments that way ("no-op until
  #37"). A comment states the rule or the missing capability itself, so
  it still reads correctly after the story is done.
- Say what minimal production code makes each test pass — cite real
  types/methods from the LLDs (§Phase 1.3), not placeholders.
- If the checkpoint adds or changes anything a user-facing doc from
  Phase 1.5 describes (a command, flag, keybinding, config format), its
  production code includes the matching doc update in the *same*
  checkpoint — not a trailing "update docs" checkpoint at the end, since
  that lets several checkpoints' worth of doc drift accumulate before
  anyone notices.
- End with a one-line **Demo**: the concrete command or test run that
  proves the checkpoint works (`cargo test -p bala-core`, or a `cargo
  run -- ...` invocation once a CLI checkpoint exists).

Close the plan with an explicit **out of scope** list: anything a reader
might reasonably expect this story to cover that it deliberately doesn't
(deferrals from Phase 2, each linking its backlog issue; capabilities
other stories own, linked; unrelated polish) — so scope is a decision on
record, not a gap someone finds later.

Present this draft to the user before publishing anything to GitHub —
confirm the checkpoint slicing, any Phase 2 scope calls, and any
milestone change from Phase 0 before they're written anywhere.

## Phase 4 — Publish to GitHub

Once the user confirms the draft:

1. **Milestone.** Apply what Phase 0 settled: create the new epic
   milestone, or append the `Scope changes:` line to the existing one's
   description. Then `gh issue edit <number> --milestone "<title>"`. For
   a new milestone:
   ```
   gh api repos/<owner>/<repo>/milestones -f title="Epic N: <Outcome>" -f description="Goal: <…> Done when: (1) <…>; (2) <…>. Scope is fixed at creation; new ideas go to the backlog. Scope changes: none."
   ```
2. **Backlog issues** from Phase 2, created before the story edit so the
   story can link them.
3. **Story issue body.** Keep the Description and ACs (with any Phase 2
   AC edits), then write the plan below them, above the Changelog:
   ```
   ---

   ## Plan
   - [ ] **Checkpoint 1 — …** …
   ...

   ## Out of scope
   - …

   ---

   ## Changelog
   - <existing entries>
   - YYYY-MM-DD — Planned: N checkpoints; scheduled into Epic N. <any AC edits and deferrals, linked>
   ```
   Use `gh issue edit <number> --body-file <path>`. Never drop or rewrite
   existing Changelog entries; only append.
4. **Revisions** follow §Changelog: append an entry, and also post a
   comment when the change is substantial.
5. End every generated body or comment with the standard attribution
   footer used elsewhere in this session (git commit / PR trailer
   convention) if the surrounding session has one active.

## Phase 5 — Hand back

Report the issue and milestone links, and note the convention for
closing the loop later: the PR that implements the story references
`Closes #<issue>` (checkpoint boxes get ticked as they land) so the
issue's progress and eventual auto-close track real work, not just the
plan.

---

## Changelog

Every story issue's body ends with a `## Changelog` section. It is the
record of how the story changed, so nobody has to reconstruct it from
edit history:

- **Log** any change to the Description, ACs, or plan scope after
  creation. Use one dated line: what changed and why, linking the PR,
  comment, or issue that triggered it.
- **Don't log** checkbox ticks or typo fixes.
- **Also post an issue comment** for substantial changes: an AC removed
  or split out, a checkpoint replanned, the story moved between
  milestones. That notifies watchers and gives the change a permalink.
  Small changes get only the Changelog line.

---

## Style notes

- Match the existing story issues' voice for anything you write into
  them: terse, AC-numbered, no filler.
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
