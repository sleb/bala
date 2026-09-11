# User Stories: Task Management App (Nested Tasks, Dependencies, Gantt)

**Product:** New task management application `bala`
**Design:** No design files provided yet — stories below are written design-agnostic; add Figma/Miro links once available.
**Assumptions:**
- Single-user or small-team app to start; no design mockups exist yet.
- "Arbitrary nesting" means a task can have subtasks, which can have their own subtasks, with no depth limit. This enables users to model "initiatives", "goals", "projects", "stories", and sub-tasks with flexibility.
- Dependencies are "finish-to-start" style (Task B can't start until Task A finishes) as the baseline case.
- Gantt chart is a read view generated from task dates/hierarchy/dependencies, not manually drawn.

---

## Epic 1: Core Task Management

### Story 1.1 — Create a Task
**Description:** As a user, I want to create a task with a title and optional details, so that I can start tracking work.

**Acceptance Criteria:**
1. User can create a task with at minimum a title (required, non-empty).
2. Optional fields at creation: description, start date, due date.
3. New task appears immediately in the task list without a page reload.
4. Empty/whitespace-only title is rejected with an inline error.
5. Created task gets a unique ID and a "created at" timestamp.
6. Task defaults to top-level (no parent) unless created from within another task.

**Note:** Assignee was originally listed as an optional field at creation (AC2), but no
story yet defines user/assignee management (no `User` entity, no assignee store or
validation). Deferred to Story 1.1a below rather than built ahead of that design.

### Story 1.1a — Assign a Task *(new, split from 1.1)*
**Description:** As a user, I want to set a task's assignee, so that it's clear who owns the work.

**Acceptance Criteria:**
1. `assignee_id` can be set at creation or via edit (Story 1.2).
2. Assignee identity/validation model (is it a free-text name, a `UserId` referencing a
   real user store, something else?) is decided here rather than assumed.
3. Assignee is visible on the task in list/detail views.

**Status:** Not yet planned — placeholder so this doesn't get silently dropped.

### Story 1.2 — Edit a Task
**Description:** As a user, I want to edit a task's details, so that I can keep it accurate as work progresses.

**Acceptance Criteria:**
1. All fields set at creation (title, description, dates, assignee) are editable afterward.
2. Changes save automatically or via an explicit "Save" action — no data loss on navigation away.
3. Edit history/updated-at timestamp is recorded.
4. Validation errors (e.g., due date before start date) are shown inline and block save.
5. Editing a task does not silently change its subtasks' dates.

### Story 1.3 — Delete a Task
**Description:** As a user, I want to delete a task, so that I can remove work that's no longer relevant.

**Acceptance Criteria:**
1. Deleting a task prompts a confirmation before removal.
2. If the task has subtasks, the user is warned and can choose to delete the whole subtree or reassign subtasks to the parent above.
3. If other tasks depend on the deleted task, the user is warned which dependent tasks will be affected.
4. Deleted task is removed from all views (list, board, Gantt) immediately.
5. Deletion is either soft (recoverable for N days) or accompanied by an "undo" toast — decision noted for later design.

### Story 1.4 — Mark a Task Complete
**Description:** As a user, I want to mark a task as done, so that I can track progress.

**Acceptance Criteria:**
1. Task has at least two states: incomplete / complete (status field extensible later).
2. Completing a task with incomplete subtasks prompts a warning or auto-completes children (behavior to be confirmed with stakeholders).
3. A completed task visually indicates completion in list, tree, and Gantt views.
4. Completing a task unblocks any dependent tasks waiting on it (see Epic 3).
5. Completion timestamp is recorded.

---

## Epic 2: Nested Task Hierarchy

### Story 2.1 — Add a Subtask to Any Task
**Description:** As a user, I want to add a subtask under any existing task, so that I can break work into smaller pieces at any depth.

**Acceptance Criteria:**
1. User can add a subtask from any task, including a subtask (i.e., nesting has no fixed depth limit).
2. New subtask inherits no fields by default except an optional prompt to inherit assignee/dates.
3. Subtask appears nested under its parent in the tree/list view, indented or visually grouped.
4. A task can be moved to become a subtask of another task (re-parenting), and vice versa (promoted to top-level).
5. Circular nesting (a task becoming its own ancestor) is prevented with a clear error.

### Story 2.2 — Collapse/Expand Task Tree
**Description:** As a user, I want to collapse and expand branches of the task tree, so that I can focus on relevant parts of a large project.

**Acceptance Criteria:**
1. Each task with subtasks shows an expand/collapse control.
2. Collapse/expand state persists per user across sessions.
3. "Expand all" / "collapse all" controls are available at the top level.
4. Collapsed parent still shows a summary indicator (e.g., "3/7 subtasks complete").
5. Deeply nested trees (5+ levels) remain navigable without performance lag on reasonable list sizes.

### Story 2.3 — Roll Up Progress from Subtasks
**Description:** As a user, I want a parent task's progress to reflect its subtasks' completion, so that I can see overall status at a glance.

**Acceptance Criteria:**
1. Parent task displays a progress indicator (e.g., percentage or fraction) computed from its subtree.
2. Progress rolls up recursively through arbitrary nesting depth.
3. Progress updates immediately when any descendant task's status changes.
4. Parent's own completion is independent of the rollup number unless explicitly configured otherwise.
5. Empty parent (no subtasks) shows no rollup or defaults to its own status.

### Story 2.4 — Label a Task's Type/Level
**Description:** As a user, I want to tag a task with a level such as Initiative, Goal, Project, Story, or Task, so that I can tell at a glance what kind of work a node in the hierarchy represents.

**Acceptance Criteria:**
1. Every task has an optional type field with selectable values (e.g., Initiative, Goal, Project, Story, Task) plus a default of "Task" if unset.
2. Type is purely descriptive — it does not restrict nesting (any type can be a parent or child of any other type).
3. Type is shown as a label, icon, or color tag in list, tree, and Gantt views.
4. User can filter or group the task tree by type (e.g., "show only Goals and their direct children").
5. Type list is editable/configurable, so teams can rename or add levels to match their own vocabulary.
6. Changing a task's type does not affect its dates, dependencies, or subtasks.

---

## Epic 3: Task Dependencies

### Story 3.1 — Define a Dependency Between Tasks
**Description:** As a user, I want to mark that one task depends on another, so that the schedule reflects real-world ordering constraints.

**Acceptance Criteria:**
1. User can select one or more predecessor tasks for a given task ("depends on").
2. A task cannot depend on itself or on any of its own descendants/ancestors (would create nesting+dependency conflicts).
3. Circular dependencies (A→B→A) are detected and rejected with a clear message.
4. Dependencies can be removed as easily as they're added.
5. Dependency relationships are visible on the task's detail view (both "blocked by" and "blocks" lists).

### Story 3.2 — Enforce Dependency Scheduling
**Description:** As a user, I want dependent tasks to automatically respect their predecessors' timing, so that I don't have to manually recalculate dates.

**Acceptance Criteria:**
1. If Task B depends on Task A, Task B's start date cannot be earlier than Task A's end date (finish-to-start).
2. Moving Task A's end date later automatically shifts Task B's dates and cascades to anything depending on B.
3. User is warned before a cascading shift affects many downstream tasks, with a preview/confirmation.
4. Manual override is possible but flags the task as "out of sync" with its dependency.
5. Completing Task A unblocks Task B for status purposes even if dates aren't touched.

### Story 3.3 — Visualize Blocked Tasks
**Description:** As a user, I want to see which tasks are currently blocked, so that I know what I can't start yet.

**Acceptance Criteria:**
1. A task whose predecessor(s) are incomplete is visually flagged as "blocked" in list and board views.
2. Blocked state clears automatically once all predecessors are complete.
3. Hovering/clicking the blocked indicator shows which specific tasks are blocking it.
4. Filter/view option to show only blocked (or only unblocked/ready) tasks.
5. Blocked status is reflected in the Gantt chart (e.g., distinct styling for blocked bars).

---

## Epic 4: Gantt Chart

### Story 4.1 — Generate a Gantt Chart from Tasks
**Description:** As a user, I want to see my tasks laid out on a Gantt chart, so that I can understand the project timeline visually.

**Acceptance Criteria:**
1. Gantt view renders every task with a start and due date as a horizontal bar positioned on a date axis.
2. Task hierarchy (nesting) is reflected as a tree on the left, matching the rest of the app.
3. Tasks missing dates are listed separately (e.g., an "unscheduled" section) rather than breaking the chart.
4. Dependency arrows are drawn between predecessor and dependent task bars.
5. Chart updates automatically when task dates, hierarchy, or dependencies change elsewhere in the app.
6. Chart supports a reasonable number of tasks (e.g., 200+) without significant lag.

### Story 4.2 — Navigate and Zoom the Gantt Chart
**Description:** As a user, I want to zoom and scroll the Gantt timeline, so that I can view anything from a single week to a multi-month project.

**Acceptance Criteria:**
1. User can switch the time scale (e.g., day / week / month view).
2. Horizontal scroll/pan moves through the timeline; a "today" marker is always identifiable.
3. A "jump to today" control recenters the view.
4. Collapsing a parent task in the tree collapses its bars into a single summary bar on the chart.
5. Zoom/scale preference persists across sessions.

### Story 4.3 — Reschedule a Task from the Gantt Chart via Keyboard (TUI)
**Description:** As a TUI user, I want to reschedule a task's bar using the keyboard, so that I can adjust dates from the Gantt view without a mouse.

**Acceptance Criteria:**
1. User can select a bar (arrow keys / focus) and enter a "reschedule" mode for it, distinguishable in the UI from normal navigation.
2. In reschedule mode, nudge keys shift both start and due date together, preserving duration; modifier keys (or a separate mode) resize start or due date independently.
3. A numeric/date-entry input is available as a faster alternative to nudging for larger date changes.
4. Changes preview the same dependency-cascade logic as editing dates in the task form (Story 3.2 AC3) before committing.
5. Invalid changes (e.g., violating a dependency) are rejected with an inline message rather than committed.
6. Change is saved and reflected in list/tree views immediately, and reschedule mode has a clear, discoverable way to exit/cancel.

### Story 4.4 — Edit Dates by Dragging on the Gantt Chart (Web/GUI)
**Description:** As a web user, I want to drag a task's bar to change its dates, so that I can reschedule without switching views.

**Acceptance Criteria:**
1. Dragging the middle of a bar moves both start and due date together, preserving duration.
2. Dragging either edge of a bar resizes start or due date independently.
3. Changes trigger the same dependency-cascade logic as editing dates in the task form (Story 3.2).
4. Invalid drags (e.g., violating a dependency) snap back or show a warning before committing.
5. Change is saved and reflected in list/tree views immediately.

### Story 4.5 — Export or Share the Gantt Chart
**Description:** As a user, I want to export the Gantt chart, so that I can share the project timeline with people outside the app.

**Acceptance Criteria:**
1. User can export the current Gantt view as an image (PNG) or PDF.
2. Export reflects current zoom level, filters, and collapse state.
3. Export includes a legend for status/blocked/dependency styling.
4. Export completes for large charts without timing out (or shows progress if long-running).
5. Exported file is named with the project/view name and export date by default.

---

These 17 stories are independently shippable and sized for roughly one sprint each. A sensible build order: **Epic 1 → Epic 2 → Epic 3 → Epic 4**, since the Gantt chart and dependency enforcement both depend on core task CRUD and hierarchy existing first. Story 4.3 (TUI) and Story 4.4 (Web/GUI) are client-specific variants of the same capability — ship whichever matches the client that exists at the time (TUI for v1, Web/GUI once the web client lands).
