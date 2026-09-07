---
name: bala-lld
description: >
  Create or revise a Bala low-level design (LLD) doc for a component, or
  the HLD itself. Use whenever the user wants to design a component, write
  or update an LLD, resolve an HLD action item / open question, plan a new
  component's schema or method contract, or address LLD PR review feedback.
  Invoke with /bala-lld.
metadata:
  internal: true
---

# Bala LLD

You are helping design one component of Bala — either a new LLD from
scratch, or a revision to an existing one (addressing review feedback,
resolving an open question, or reflecting a decision made in a sibling
LLD). Output is a single markdown doc:

- `docs/design/<component-slug>.md` — e.g. `core-library.md`,
  `data-store.md`, `cli-tui-client.md`

Unlike some design workflows, Bala does **not** split "design" from
"plan" into separate files, and does not use per-release subfolders.
Each component gets exactly one flat file directly under `docs/design/`,
carrying both the design decisions and (in its final `Action Items`
section) the build checklist. There is no dedicated template file —
`docs/design/core-library.md`, `docs/design/data-store.md`, and
`docs/design/cli-tui-client.md` are themselves the template: read at
least one in full before drafting, and match its structure and register
exactly.

Work through the phases below in order.

---

## Phase 0 — New LLD or revision?

Check `docs/design/` first. If a file for this component already exists,
this is a **revision** — skip to Phase 4 with that file loaded. Otherwise
it's a **new LLD** — continue to Phase 1.

Revisions happen for one of these reasons; ask the user which if it isn't
obvious from context:

1. Addressing PR review feedback on the doc itself
2. A sibling LLD (or the HLD) pinned down something this doc had left open
3. A decision here needs to change because reality diverged during
   implementation

---

## Phase 1 — Understand the scope

Ask the user, if not already clear:

1. Which component is this? (must match one of HLD.md's components, or be
   a deliberate addition to the architecture — confirm with the user
   before inventing a new one)
2. Which HLD action item(s) / deferred item(s) does this LLD resolve?

Then read, in this order:

- `docs/design/HLD.md` — find this component's row in **Component
  responsibilities**, its contract(s) in **Interfaces / Contracts**, and
  its bullet in **Deferred to LLDs**. These three are what this LLD must
  resolve; don't re-litigate decisions the HLD already made (component
  boundaries, top-level data shape) — only what it explicitly deferred.
- `docs/design/STORIES.md` — read the epic(s) this component serves. Note
  exact story/AC numbers you'll cite (e.g. "Story 3.2 AC3").
- Every existing file in `docs/design/*.md` whose **Decision** section
  defines a trait, struct, or contract this new LLD will implement or
  call. A component consuming another's boundary (e.g. Data Store
  implementing Core Library's `Store` trait) must treat that boundary as
  fixed, not renegotiate it here — if it's wrong, that's a finding to
  raise back to the upstream LLD (see §Resolved Since First Draft below),
  not a silent local deviation.

## Phase 2 — Determine this LLD's number

LLDs are numbered sequentially by creation order, independent of the
HLD's own numbering (which stays `HLD-1`): find the highest `LLD-N` in
any existing `docs/design/*.md` title and use `N+1`. Grep for `^# LLD-`
across the directory rather than trusting file order.

## Phase 3 — Draft the design

Write `docs/design/<component-slug>.md`. Use kebab-case for the slug,
matching the component name (e.g. "CLI/TUI Client" → `cli-tui-client`).

Follow this structure, matching `core-library.md` / `data-store.md` /
`cli-tui-client.md` section-for-section:

**Header** — title line `# LLD-N: Bala — <Component> Low-Level Design`,
then `**Status:** Proposed`, `**Date:**` (today, ISO), `**Deciders:**
Scott (product/eng)`, `**Related:**` — link `HLD.md` naming the component
section it implements, plus any sibling LLD this one calls into or is
called by, plus `STORIES.md` naming the epic(s). Link sibling LLDs by
their actual filename (e.g. `core-library.md`), not an invented one —
check the file actually exists at that path before writing the link.

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
sibling LLDs, not placeholders. Inline comments explain *why* a field or
index exists, tying back to a specific story AC or an upstream trait
requirement wherever one applies.

**Error Taxonomy** — omit only if the component truly has no fallible
operations. A `thiserror` enum, each variant with a doc comment; if this
component wraps a lower layer's errors (e.g. `Store` errors inside
`CoreError`), show the `#[from]` wrapping explicitly.

**Testing Strategy** — prose + bullets, not a table (this repo's LLDs use
prose here, unlike knap's tabular test lists). Name actual test function
names following the repo's `should_` convention (e.g.
`add_parent_edge_should_be_idempotent_on_duplicate`), grouped by what
property or module they cover. Call out any atomicity, concurrency, or
scale (e.g. "200+ tasks") property the design depends on, with a test
that specifically exercises it.

**Resolved Since First Draft** — include only on a revision (Phase 0 case
1 or 2): a numbered list of what changed, why (cite the review comment or
upstream LLD), and confirm no schema/trait change was needed if that's
true. Omit entirely on a first draft — don't write "N/A".

**Deferred to Other LLDs** — bullets naming the sibling LLD and exactly
what's left to it. If everything the HLD asked this LLD to resolve is
resolved, say so explicitly rather than leaving the section thin.

**Consequences** — bullets, trade-offs and their scope of impact ("fine
at hundreds of tasks", "future caller must..."). This is where you name
what the design does *not* solve and why that's acceptable now.

**Action Items** — a numbered checklist (`- [ ]`) of concrete
implementation steps in build order (data model → lower-level queries/
modules → higher-level assembly → tests last, mirroring the layering the
Decision section just described). Each item names an actual
crate/module/function to create. This doubles as the file's plan — there
is no separate plan document.

After drafting, review with fresh eyes: does every section reference
real types/methods (grep sibling LLDs to confirm, don't guess); is any
decision asserted without a tied-back reason; does the mermaid diagram's
edges match the prose; is every HLD "deferred to this LLD" item actually
addressed somewhere.

## Phase 4 — Revision-specific steps

If this is a revision (Phase 0):

1. Add or extend **Resolved Since First Draft** describing exactly what
   changed and citing the trigger (review comment text, or the upstream
   LLD section that pinned the value down).
2. If the change touches a trait/schema another LLD depends on, check
   that sibling file's own references to this component and flag to the
   user if it now needs a matching update — don't silently edit another
   component's file without asking.
3. Leave `**Status:**` as `Proposed` unless the user says the doc is
   final.

## Phase 5 — Close the loop with the HLD

If this LLD resolves an HLD action item or a "Deferred to LLDs" bullet,
propose (don't apply without confirming) the matching edit to
`docs/design/HLD.md`:

- Check off the relevant **Action Items** entry (`- [x]`), following the
  existing pattern (e.g. "Write Core Library LLD (...) — resolved:
  ..." isn't the format used; check the current file for the exact
  phrasing style before editing).
- If the LLD resolved an "Open Question" the HLD listed as unresolved,
  update that bullet in place to state the resolution and which LLD/
  section made it, matching the phrasing pattern already used for
  resolved items in HLD.md (e.g. "resolved: soft-delete, per Core
  Library LLD §Context").

Then present the new/updated LLD to the user with a short summary: what
it resolves, the key decisions and why, and any open question it raises
back to a sibling LLD or the HLD that the user should weigh in on.

---

## Style and tone

Match the existing docs exactly — read one in full before writing a word:

- Dense, justified prose. Every non-obvious decision gets a "because X"
  tied to a concrete upstream constraint (a fixed trait signature, a
  story AC, a stated scale target) — never asserted by convention alone.
- Real Rust/SQL in code blocks, using actual names from sibling LLDs —
  verify a referenced type/method actually exists in the LLD that defines
  it before citing it.
- One mermaid diagram in `## Decision` showing this component's internal
  modules and its edges to neighboring components.
- Sections are prose-first; tables only where the existing docs already
  use them (none do for testing — that's prose+bullets here, unlike
  knap's tabular convention).
- `**Related:**` links must point at files that actually exist at that
  path — check before writing a link.
- Close every LLD's loop back to the HLD explicitly (Phase 5) — a
  component doc that resolves an open question but leaves the HLD saying
  "deferred" is a doc drifting out of sync with itself.
