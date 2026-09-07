---
name: bala-lld
description: >
  Create or revise a Bala low-level design (LLD) for a component or
  feature, or the HLD itself. Use whenever the user wants to design a
  component or feature, write or update a design doc, resolve an HLD
  action item / open question, plan a new component's schema or method
  contract, or address design/plan PR review feedback. Invoke with
  /bala-lld.
metadata:
  internal: true
---

# Bala LLD

You are helping design one component or feature of Bala — either a new
design from scratch, or a revision to an existing one (addressing review
feedback, resolving an open question, or reflecting a decision made in a
sibling design). Output is a directory per component/feature, holding two
files:

- `docs/design/<slug>/design.md` — decisions, schema/method contracts,
  rationale
- `docs/design/<slug>/plan.md` — the action-item checklist that
  implements the design

e.g. `docs/design/data-store/design.md` +
`docs/design/data-store/plan.md`. Design and plan are always split into
these two separate files; never merge them back into one.

There is no dedicated template file. `docs/design/core-library.md`,
`docs/design/data-store.md`, and `docs/design/cli-tui-client.md` predate
the directory-per-component/design+plan split — they're still the
reference for prose register, level of technical detail, and how deeply
to justify decisions, but their *file layout* (single flat file, `LLD-N`
numbering) is the old convention. Don't number new designs — a component
or feature is named for what it is (`data-store`, `tui`, `core-library`),
not by creation order. New work uses the directory + design.md/plan.md
layout described here regardless of what those legacy files look like.

Work through the phases below in order.

---

## Phase 0 — New design or revision?

Check `docs/design/` first. If a directory (or legacy single file) for
this component or feature already exists, this is a **revision** — skip
to Phase 4 with `design.md` and `plan.md` loaded. Otherwise it's a **new
design** — continue to Phase 1.

Revisions happen for one of these reasons; ask the user which if it isn't
obvious from context:

1. Addressing PR review feedback on the doc itself
2. A sibling design (or the HLD) pinned down something this doc had left
   open
3. A decision here needs to change because reality diverged during
   implementation

---

## Phase 1 — Understand the scope

Ask the user, if not already clear:

1. Which component or feature is this? (must match one of HLD.md's
   components, or be a deliberate addition to the architecture — confirm
   with the user before inventing a new one)
2. Which HLD action item(s) / deferred item(s) does this design resolve?

Then read, in this order:

- `docs/design/HLD.md` — find this component's row in **Component
  responsibilities**, its contract(s) in **Interfaces / Contracts**, and
  its bullet in **Deferred to LLDs**. These three are what this design
  must resolve; don't re-litigate decisions the HLD already made
  (component boundaries, top-level data shape) — only what it explicitly
  deferred.
- `docs/design/STORIES.md` — read the epic(s) this component serves. Note
  exact story/AC numbers you'll cite (e.g. "Story 3.2 AC3").
- Every existing `design.md` (or legacy `docs/design/*.md`) whose
  **Decision** section defines a trait, struct, or contract this new
  design will implement or call. A component consuming another's boundary
  (e.g. Data Store implementing Core Library's `Store` trait) must treat
  that boundary as fixed, not renegotiate it here — if it's wrong, that's
  a finding to raise back to the upstream design (see §Resolved Since
  First Draft below), not a silent local deviation.

## Phase 2 — Draft the design

Write `docs/design/<slug>/design.md`. Use kebab-case for the slug,
matching the component/feature name (e.g. "CLI/TUI Client" →
`cli-tui-client`).

Follow this structure, matching the register and depth of
`core-library.md` / `data-store.md` / `cli-tui-client.md`
section-for-section:

**Header** — title line `# Bala — <Component/Feature> Design`, then
`**Status:** Proposed`, `**Date:**` (today, ISO), `**Deciders:** Scott
(product/eng)`, `**Related:**` — link `HLD.md` naming the component
section it implements, plus any sibling design this one calls into or is
called by (linking to that design's `design.md`), plus `STORIES.md`
naming the epic(s). Link sibling designs by their actual path (e.g.
`../core-library/design.md`), not an invented one — check the file
actually exists at that path before writing the link.

**Context** — one paragraph restating what the HLD fixed for this
component and what it explicitly deferred here (cite the HLD's own
"Deferred to LLDs" bullet). Then a short bulleted list of the decisions
*this* doc resolves that are load-bearing enough to flag up front —
typically language/engine/library choices that shape the whole method
surface, each with a one-paragraph justification citing the actual
constraint (a trait signature already fixed elsewhere, a story's
performance/UX requirement, etc.) — never justify by convention or taste
alone.

**Decision** — state the crate/module/type layout in prose, then a
mermaid diagram (`flowchart TB` or `LR`) showing the internal module
breakdown and its edges to sibling components. Follow with prose
explaining any non-obvious structural choice (e.g. why a `RefCell` and
not a `Mutex`) — tie every such choice back to a constraint fixed
upstream (a trait signature, a single-writer assumption) rather than
asserting it.

**Data Model / Schema / Method Contract(s)** (name and count these
sections to fit the component — a store gets `## Schema` with SQL DDL, a
library gets `## Data Model` with Rust types and `## Method Contract`
with signatures, a client gets `## Screens` or `## Rendering Model`).
Use real, compilable-looking Rust (or SQL) — actual type names from
sibling designs, not placeholders. Inline comments explain *why* a field
or index exists, tying back to a specific story AC or an upstream trait
requirement wherever one applies.

**Error Taxonomy** — omit only if the component truly has no fallible
operations. A `thiserror` enum, each variant with a doc comment; if this
component wraps a lower layer's errors (e.g. `Store` errors inside
`CoreError`), show the `#[from]` wrapping explicitly.

**Testing Strategy** — Bala is strictly TDD: every module or method
contract this doc introduces must have its tests identified *before* the
corresponding plan.md implementation step, and the plan (Phase 3) must
sequence writing each test ahead of the code that satisfies it — never
list "write tests" as a trailing cleanup step. Name actual test function
names following the repo's `should_` convention (e.g.
`add_parent_edge_should_be_idempotent_on_duplicate`). A table is fine
here (columns like Test name / Exercises / Property) when the test list
is long or benefits from scanning by module; use prose+bullets when a
handful of tests read better as narrative. Either way, call out any
atomicity, concurrency, or scale (e.g. "200+ tasks") property the design
depends on, with a test that specifically exercises it.

**Resolved Since First Draft** — include only on a revision (Phase 0 case
1 or 2), and only as a short pointer for reviewers to what prompted the
revision (cite the review comment or upstream design) — it is not a
running changelog. The rest of the doc must read as the *current* design,
not an annotated history: see Phase 4 for how to fold a revision in
without leaving stale cruft behind. Omit entirely on a first draft — don't
write "N/A".

**Deferred to Other Designs** — bullets naming the sibling design and
exactly what's left to it. If everything the HLD asked this design to
resolve is resolved, say so explicitly rather than leaving the section
thin.

**Consequences** — bullets, trade-offs and their scope of impact ("fine
at hundreds of tasks", "future caller must..."). This is where you name
what the design does *not* solve and why that's acceptable now.

After drafting, review with fresh eyes: does every section reference
real types/methods (grep sibling designs to confirm, don't guess); is any
decision asserted without a tied-back reason; does the mermaid diagram's
edges match the prose; is every HLD "deferred to this design" item
actually addressed somewhere.

## Phase 3 — Draft the plan

Write `docs/design/<slug>/plan.md`: a numbered checklist (`- [ ]`) of
concrete implementation steps in build order (data model → lower-level
queries/modules → higher-level assembly), with each test from the design's
Testing Strategy sequenced immediately *before* the implementation step it
drives, per Bala's strict-TDD discipline — never batch tests at the end.
Each item names an actual crate/module/function to create or test to
write. This file is the sole plan for the component/feature — design.md
does not duplicate it.

## Phase 4 — Revision-specific steps

If this is a revision (Phase 0):

1. Edit `design.md` and `plan.md` in place so they read as the current,
   correct design — do not preserve superseded decisions, stale open
   questions, or dead options "for history." If a decision changed,
   replace the old text with the new one; the record of *that it
   changed* belongs in the PR/commit history and in the brief
   **Resolved Since First Draft** pointer, not scattered through the
   doc's body as leftover cruft.
2. Add or extend **Resolved Since First Draft** with a short pointer:
   what changed and the trigger (review comment text, or the upstream
   design section that pinned the value down) — not a full narrative.
3. If the change touches a trait/schema another design depends on, check
   that sibling file's own references to this component and flag to the
   user if it now needs a matching update — don't silently edit another
   component's file without asking.
4. Leave `**Status:**` as `Proposed` unless the user says the doc is
   final.

## Phase 5 — Close the loop with the HLD

If this design resolves an HLD action item or a "Deferred to LLDs"
bullet, propose (don't apply without confirming) the matching edit to
`docs/design/HLD.md`:

- Check off the relevant **Action Items** entry (`- [x]`), following the
  existing pattern (check the current file for the exact phrasing style
  before editing).
- If the design resolved an "Open Question" the HLD listed as unresolved,
  update that bullet in place to state the resolution and which
  design/section made it, matching the phrasing pattern already used for
  resolved items in HLD.md (e.g. "resolved: soft-delete, per Core Library
  design §Context").

Then present the new/updated design and plan to the user with a short
summary: what it resolves, the key decisions and why, and any open
question it raises back to a sibling design or the HLD that the user
should weigh in on.

---

## Style and tone

Match the existing docs' register exactly — read one in full before
writing a word:

- Dense, justified prose. Every non-obvious decision gets a "because X"
  tied to a concrete upstream constraint (a fixed trait signature, a
  story AC, a stated scale target) — never asserted by convention alone.
- Real Rust/SQL in code blocks, using actual names from sibling designs —
  verify a referenced type/method actually exists in the design that
  defines it before citing it.
- One mermaid diagram in `## Decision` showing this component's internal
  modules and its edges to neighboring components.
- Sections are prose-first; a table is fine for Testing Strategy when the
  test list is long, otherwise prose+bullets.
- `**Related:**` links must point at files that actually exist at that
  path — check before writing a link.
- Testing is strictly TDD: plan.md always sequences a test immediately
  before the code it drives, never after.
- Close every design's loop back to the HLD explicitly (Phase 5) — a
  component doc that resolves an open question but leaves the HLD saying
  "deferred" is a doc drifting out of sync with itself.
- On a revision, leave the doc reading as the single current truth — no
  annotated history of superseded decisions left in place (Phase 4).
