# Bala CLI

`bala` is a command-line task tracker with both a scriptable subcommand set
and an interactive terminal UI. This document covers what's implemented so
far.

## Database location

Commands operate on a SQLite database. By default it lives in the
OS-standard data directory for the `bala` app (e.g.
`~/.local/share/bala/bala.db` on Linux), created automatically on first use.

Override the path with the global `--db-path` flag, mainly useful for tests
or working with multiple task lists:

```sh
bala --db-path ./my-project.db task ls
```

See [config.md](config.md) for the full path-resolution details and default
locations per OS.

## Running with no subcommand

```sh
bala
```

Launches an interactive full-screen terminal UI showing your tasks,
rather than dispatching to one of the subcommands below.

The task list shows every task's title, type, status, and assignee,
indented under its parent: a subtask is nested two spaces deeper than its
parent. A task has at most one parent, so every task appears as exactly one
row. A task with subtasks shows a `▾` (expanded) or `▸`
(collapsed) glyph before its checkbox; a collapsed task's row also shows a
`(complete/total)` summary counting its direct children only. A task that
is waiting on an incomplete predecessor shows a red `⊘` right after its
checkbox; the glyph disappears as soon as the last incomplete predecessor is
completed. A completed task is never shown as blocked. Collapse/
expand state persists across sessions in `view.toml` (see
[config.md](config.md)). An empty task list shows a "No tasks yet." message
instead of a blank screen.

| Key | Action |
| --- | --- |
| `j` / `↓` (in the list) | Move the selection down. |
| `k` / `↑` (in the list) | Move the selection up. |
| `h` / `←` (in the list) | Collapse the focused task, hiding its subtasks. No-op if it has none, or is already collapsed. |
| `l` / `→` (in the list) | Expand the focused task, revealing its subtasks again. No-op if it isn't collapsed. |
| `E` (in the list) | Expand every task. |
| `C` (in the list) | Collapse every task that has subtasks. |
| `J` / `K` (in the list) | Move the focused task down / up among its siblings. Selection stays on the moved task. No-op at the end of the list. With a type filter active the swap uses the full sibling order, so a press can look like a no-op when the neighbor is hidden. |
| `L` / `H` (in the list) | Indent / outdent the focused task. `L` nests it as the last child of its previous sibling (which is expanded so the task stays visible); `H` moves it to its parent's level, right after that parent. Its subtree moves with it. Selection stays on the moved task. No-op with no previous sibling / at the top level, and on a row whose parent the type filter hides (it is shown at the top level, but is not really there). Distinct from lowercase `h`/`l` (collapse/expand). |
| `O` | Create a new top-level task: opens a text-entry line for its title. `Enter` submits, `Esc` cancels. |
| `o` (in the list) | Create a new subtask under the selected task: opens a text-entry line for its title, nested under the focused task. `Enter` submits, `Esc` cancels. If the parent has an assignee or dates, you're then asked whether to inherit them onto the new subtask (`y`/`n`). |
| `m` (in the list) | Reparent the selected task: opens a text-entry line prefilled with its current parent id (empty when it is top-level). `Enter` submits, `Esc` cancels. |
| `t` (in the list) | Set the selected task's type: opens a text-entry line prefilled with its current type key. `Enter` submits, `Esc` cancels. |
| `p` (in the list or Detail pane) | Add a dependency to the selected task: opens an empty text-entry line for the id of the task it depends on. `Enter` submits, `Esc` cancels. |
| `P` (in the list or Detail pane) | Remove a dependency from the selected task: opens a text-entry line for the id of the task it should no longer depend on, prefilled when the task has exactly one dependency. `Enter` submits, `Esc` cancels. |
| `f` (in the list) | Cycle the active type filter: no filter → the first configured type → the next → ... → back to no filter. Persists across sessions in `view.toml` (see [config.md](config.md)). |
| `b` (in the list) | Cycle the blocked/ready view: all → blocked (tasks waiting on an incomplete predecessor) → ready (incomplete tasks that are not blocked) → all. Combines with the type filter. The active view shows in a `view: …` status line below the list; a row whose parent the view hides is shown at the top level. |
| `Enter` (in the list) | Open the Detail pane for the selected task. |
| `j` / `↓` (in the Detail pane) | Move the field cursor down (Title → Description). |
| `k` / `↑` (in the Detail pane) | Move the field cursor up (Description → Title). |
| `i` (in the Detail pane) | Edit the field under the cursor: opens a text-entry line prefilled with its current value. `Enter` submits, `Esc` cancels. |
| `Esc` (in the Detail pane) | Leave the Detail pane, back to the list. |
| `dd` (in the list) | Delete the selected task: press `d` twice in a row to prompt for confirmation. For a task with no subtasks, `y` confirms and `n`/`Esc` cancels. For a task with subtasks, the prompt names how many it has: `s` deletes the task and its whole subtree, `p` deletes only the task and promotes its subtasks to its parent (or to top level if it had none), and `n`/`Esc` cancels. If other tasks depend on the task, or on its subtasks, the prompt lists them above the question. |
| `x` / `Space` (in the list or Detail pane) | Toggle the selected task's complete/incomplete state. Completing a task with incomplete subtasks prompts for confirmation: `y` completes the whole cascade, `n`/`Esc` cancels. |
| `?` | Open the help overlay, listing every key bound in the current mode. Not available in Insert mode, where `?` types a literal question mark — use `F1` there instead. |
| `F1` (in a text-entry line) | Open the help overlay. |
| `Esc` (in the help overlay) | Close the overlay and return to whatever you were doing. |
| `q` | Quit, restoring the terminal to its normal state. |

Selection stops at the first/last row rather than wrapping around.

Pressing `O` opens a `New task: ` input line at the bottom of the screen.
Type the title and press `Enter` to create it (it's appended to the list and
selected), or `Esc` to cancel without creating anything. An empty title is
rejected with an inline error message shown under the input line; fix the
title and press `Enter` again, or `Esc` to give up.

Pressing `o` on a selected task opens a `New subtask: ` input line at the
bottom of the screen, scoped to that task as the new subtask's parent. Type
the title and press `Enter` to create it — it's nested under the focused
task in the list and selected — or `Esc` to cancel without creating
anything. An empty title is rejected the same way as `O`'s new-task entry.
If the parent has an assignee and/or start/due dates set, submitting the
title then prompts: "Inherit assignee/dates from parent...? (y/n)" — `y`
copies those fields onto the new subtask, `n` leaves it unassigned and
undated. When the parent has none of those fields set, the new subtask is
created directly with no prompt.

Pressing `m` on a selected task opens a `Parent id: ` input line at the
bottom of the screen, prefilled with the task's current parent id (empty
when it's already top-level). Edit the id and press `Enter` to submit, or
`Esc` to cancel without changing anything. Submitting one id moves the task
under that parent; submitting an empty line promotes the task to top-level
(no parent). A task has at most one parent, so entering more than one id
(separated by commas or spaces) is an error. That, an id that isn't a valid
task id, or a reparent that would make the task its own ancestor (a circular
hierarchy), shows an inline error under the input line and keeps you in the
entry line to fix it — fix the id and press `Enter` again, or `Esc` to give
up. `m` does not offer to inherit the new parent's assignee or dates; that's
`o`'s behavior for newly created subtasks only.

Pressing `t` on a selected task opens a `Type: ` input line at the bottom of
the screen, prefilled with the task's current type key. Edit it and press
`Enter` to submit, or `Esc` to cancel without changing anything. A key that
doesn't name a configured task type (see `bala type set`/`bala type ls`)
shows an inline error under the input line and keeps you in the entry line
to fix it — fix the key and press `Enter` again, or `Esc` to give up.

Pressing `p` on a selected task, in the list or the Detail pane, opens an
empty `Depends on (task id): ` input line at the bottom of the screen. Enter
the one full id of the task it should depend on and press `Enter` to add the
dependency, or `Esc` to cancel without changing anything. The dependency is
always finish-to-start; use `bala dep add --type` for the other types. Press
`p` again to add another predecessor. If the predecessor is incomplete, the
task's row gains the `⊘` blocked marker.

A dependency that can't be added shows a message under the input line and
keeps you in the entry line with what you typed — fix the id and press
`Enter` again, or `Esc` to give up. Nothing is changed until an entry is
accepted. The entry is rejected if it:

- is empty, is not a valid task id, or holds more than one id;
- names no task (`no task with that id`);
- names the task itself (`a task cannot depend on itself`);
- names a task this one already depends on
  (`this task already depends on that task`), so an existing dependency is
  never silently rewritten as finish-to-start;
- names an ancestor or a descendant of the task, such as its parent or one of
  its subtasks (`"Parent" is an ancestor or descendant of this task`);
- would close a cycle, because that task already depends on this one,
  directly or through other tasks. The message names the tasks in the cycle
  by title: with B depending on A, making A depend on B shows
  `would create a cycle: A → B → A`. Tasks hidden by the type filter are
  named by title too.

Pressing `P` on a selected task, in the list or the Detail pane, opens a
`Remove dependency on (task id): ` input line at the bottom of the screen.
When the task depends on exactly one task, the line is prefilled with that
task's id, so `Enter` alone removes the dependency; with several (or none)
it opens empty, and you enter the one full id of the task it should stop
depending on. `Esc` cancels without changing anything. Once the last
incomplete predecessor is removed, the task's row loses the `⊘` blocked
marker. The line is prefilled with the only dependency's id even when that
predecessor has been deleted: "Blocked by" does not list a deleted
predecessor, and this is how a dependency on one is cleared.

As with `p`, an entry that is empty, is not a valid task id, or holds more
than one id shows a message under the input line and keeps you in the entry
line with what you typed. So does the id of a task the selected task does
not depend on (`this task does not depend on that task`), so a mistyped id
is never mistaken for a successful removal.

Pressing `Enter` on a task in the list opens its Detail pane, showing the
task's title and description with the field cursor (highlighted) starting on
the title. `j`/`k` move the cursor between the two fields (the same physical
keys used to move the list selection, but scoped to the Detail pane while
it's open); `i` opens a text-entry line prefilled with the focused field's
current value — `Enter` saves the change and returns to the Detail pane
showing the new value, `Esc` discards it. An empty title is rejected the
same way as in `O`'s new-task entry (inline error, stays in the entry line);
an empty description is accepted. `Esc` in the Detail pane itself (not
mid-edit) leaves it and returns to the list. The pane also closes back to
the list when an edit made in it (`x`, `p` or `P`) moves the task out of the
active blocked/ready view, since the task is then no longer listed.

When the selected task depends on at least one task that has not been
deleted, the Detail pane also shows a "Blocked by" section below the
description: one line per predecessor, complete or not, giving its title and
status (e.g. `Design (incomplete)`), in dependency order. A finished
predecessor stays listed and reads `Design (complete)`; a deleted one is not
listed. Predecessors are looked up directly, so one hidden by the type
filter is still listed. The section is omitted entirely when the task has no
predecessors to list.

Under it, a "Blocks" section lists every task that depends on the selected
one and has not been deleted: one line per dependent, complete or not, giving
its title and status (e.g. `Build (incomplete)`), in list order. Dependents
are found across all tasks, so one hidden by the type filter is still listed.
The section is omitted entirely when nothing depends on the task.

Pressing `d` twice in a row on a selected task shows a confirmation prompt
naming the task, at the bottom of the screen. For a task with no subtasks,
press `y` to delete it or `n`/`Esc` to cancel without deleting anything. For
a task with subtasks, the prompt says how many direct subtasks it has
(counting any hidden by the type filter or a collapsed row) and asks how to
treat them, matching `bala task delete`'s `--cascade` and
`--promote-children`: press `s` to delete the task along with its whole
subtree (every subtask at any depth), `p` to delete only the task and move
its direct subtasks up to the task's own parent (or to top level if it had
none), or `n`/`Esc` to cancel. `y` does nothing at this prompt. Pressing `d` once and then any other key
(rather than a second `d`) does not count toward the sequence — the next `d`
starts fresh.

When other tasks depend on the task, the prompt lists them above the
question:

```text
2 task(s) depend on this task:
  Write docs
  Ship release
1 more depend on its subtasks (subtree delete only):
  Announce
"Build" has 2 subtask(s). Delete [s]ubtree, [p]romote children, or [n] cancel?
```

The first group is the tasks that depend on the task itself. The second
appears only for a task with subtasks: the other tasks, outside its subtree,
that depend on a subtask at any depth. Those are affected only if you delete
the subtree with `s`; `p` keeps the subtasks. Each group shows at most five
titles, then `… and N more` for the rest; the count in its first line is the
whole group. Tasks hidden by the type filter are included. A group with no
tasks is left out, so a task nothing depends on gets the question alone. The
list is a warning only: the delete itself is unaffected, and the dependency
edges stay, as [`bala task delete`](#bala-task-delete) explains.

The TUI requires a real terminal (a TTY) on stdin: running it in a
non-interactive context (e.g. piped/redirected input, as CI or scripting
harnesses do) fails immediately with a "terminal I/O error" message and a
nonzero exit code, rather than hanging.

## `bala user`

### `bala user add`

Creates a new user.

```sh
bala user add --name "Ada Lovelace"
```

| Flag | Required | Description |
| --- | --- | --- |
| `--name` | yes | The user's display name. |

Prints the new user's id (a UUID) on success.

### `bala user ls`

Lists all users.

```sh
bala user ls
```

Output is one user per line: `<id> <name>`.

## `bala task`

### `bala task add`

Creates a new task.

```sh
bala task add --title "Write docs" \
  --description "Document the CLI commands" \
  --parent <parent-task-id> \
  --start 2026-09-15 \
  --due 2026-09-20 \
  --duration 5 \
  --assignee <user-id> \
  --type task
```

| Flag | Required | Description |
| --- | --- | --- |
| `--title` | yes | The task's title. |
| `--description` | no | A longer description of the task. |
| `--parent` | no | Id of the task to nest the new task under. A task has at most one parent: giving `--parent` more than once is a usage error and creates nothing. |
| `--start` | no | Start date, `YYYY-MM-DD`. |
| `--due` | no | Due date, `YYYY-MM-DD`. |
| `--duration` | no | Duration in whole calendar days (`due = start + duration`); `0` is a milestone. A negative or non-numeric value is a usage error. It is not checked against `--start`/`--due`; [`bala schedule`](#bala-schedule) uses it to place a floating task's dates. |
| `--assignee` | no | Id of the user to assign the task to. |
| `--inherit` | no | Copy assignee/start/due from the `--parent` task for any of those fields not explicitly given. Only meaningful alongside `--parent`; passing it without `--parent` is a usage error. |
| `--type` | no | Task type key. Defaults to the seeded `task` type when omitted. Fails with a nonzero exit if the key doesn't name a configured task type. |

`--inherit` fills in `--assignee`/`--start`/`--due` from the new task's
`--parent` wherever the corresponding flag wasn't given explicitly — an
explicit flag always wins.

A task created with a start or due date (given explicitly or inherited) has
its dates fixed; a task created with neither is not. `--duration` alone does
not fix a task. Use [`bala task edit`](#bala-task-edit)'s `--float` to
release a task's dates afterwards.

Work that more than one goal needs still has a single parent: nest it under
one goal and make the others depend on it with [`bala dep add`](#bala-dep-add).
A finish-to-finish dependency (`--type ff`) says "this goal is not done until
that task is done".

Prints the new task's id (a UUID) on success.

### `bala task ls`

Lists all tasks.

```sh
bala task ls
bala task ls --type task
```

| Flag | Required | Description |
| --- | --- | --- |
| `--type` | no | Only show tasks with this type key. Filtering by a key no task currently has (including one that isn't configured) simply shows no tasks — it's a plain filter, not an existence check. |

Output is one task per line: `[<marker>] <id> <title> (type: <type-key>)`,
followed, in this order, by each of these that applies:

- ` (assigned: <name>)` when the task has an assignee;
- ` <start>..<due>` when either date is set, as `YYYY-MM-DD`, with an unset
  side printed as `?` (`2026-10-05..?`);
- ` (<n>d)` when the task has a duration;
- ` (fixed)` when the task's dates are fixed;
- ` (out of sync)` when the task's dates break one of its dependencies —
  for the default `fs` type, when it starts before a task it depends on is
  due. Starting on that very day is fine. A dependency is not checked while
  a date it compares is unset or its predecessor is deleted. A completed
  task is never marked out of sync, whatever its dates.

A task with no dates and no duration prints none of the last four.
`<marker>` is `x` for a completed task and a space otherwise. Every task is
printed on exactly one line. A task with a parent is indented one level; this is a minimal visual cue, not a full recursive
tree layout (that's planned for the TUI).

### `bala task edit`

Edits an existing task. Only the fields you pass are changed; everything
else is left as-is.

```sh
bala task edit <task-id> \
  --title "Write docs" \
  --description "Document the CLI commands" \
  --start 2026-09-15 \
  --due 2026-09-20 \
  --duration 5 \
  --assignee <user-id> \
  --type <type-key>
```

| Flag | Description |
| --- | --- |
| `--title` | New title. |
| `--description` | New description. |
| `--clear-description` | Clear the description. Conflicts with `--description`. |
| `--start` | New start date, `YYYY-MM-DD`. |
| `--clear-start` | Clear the start date. Conflicts with `--start`. |
| `--due` | New due date, `YYYY-MM-DD`. |
| `--clear-due` | Clear the due date. Conflicts with `--due`. |
| `--duration` | New duration in whole calendar days; `0` is a milestone. A negative or non-numeric value is a usage error. |
| `--clear-duration` | Clear the duration. Conflicts with `--duration`. |
| `--fix` | Fix the task's dates. Conflicts with `--float`. |
| `--float` | Float the task's dates. Conflicts with `--fix`. |
| `--assignee` | Id of the user to assign the task to. |
| `--clear-assignee` | Unassign the task. Conflicts with `--assignee`. |
| `--type` | New task type key. |

Setting `--start` or `--due` fixes the task's dates. `--float` in the same
command overrides that, so `--start 2026-10-05 --float` records a start date
the task is not held to; `--fix` and `--float` on their own change only
whether the existing dates are fixed. A task with no dates is never fixed:
`--fix` on one has no effect, and clearing a task's last date floats it.
Changing the duration never fixes or floats a task.

Prints the edited task in the same format as `task ls`. Editing a task
never changes another one: a task left breaking a dependency by the edit
prints as ` (out of sync)` until [`bala schedule`](#bala-schedule) moves it
or its dates are changed by hand.

### `bala task mv`

Moves a task under a new parent, replacing its current one, or promotes it
to top-level.

```sh
bala task mv <task-id> --parent <parent-id>   # move under that parent
bala task mv <task-id>                        # promote to top-level
```

| Flag | Required | Description |
| --- | --- | --- |
| `--parent` | no | Id of the new parent. Omit to promote the task to top-level (no parent). A task has at most one parent: giving `--parent` more than once is a usage error and moves nothing. |

Prints the moved task, in the same format as `task ls`.

If the task id or the given parent id doesn't exist, or if the move would
make the task its own ancestor (for example naming the task itself, or one
of its own descendants, as the new parent), the command fails with a nonzero
exit and an error message describing the problem, and the task stays where
it was.

### `bala task delete`

Deletes a task, prompting for confirmation unless `--yes` is given.

```sh
bala task delete <task-id> [--yes] [--cascade | --promote-children]
```

| Flag | Description |
| --- | --- |
| `--yes` | Skip the interactive confirmation prompt. |
| `--cascade` | Delete the task's whole subtree along with it: every subtask at any depth is deleted too. Conflicts with `--promote-children`. |
| `--promote-children` | Delete only the task and move its children to its own parent, or to top level if it had none. |

If the task has subtasks and neither `--cascade` nor `--promote-children` is
given, the command fails and lists the subtasks so you can choose a mode.

If other live tasks depend on what the delete covers, their ids and titles
are printed to stderr before the confirmation prompt (and also with `--yes`):

```text
2 task(s) depend on what this deletes:
  <task-id> <title>
  <task-id> <title>
```

With `--cascade` (or a task with no subtasks) this covers the task and its
whole subtree: every task outside the subtree that depends on the task or on
any of its subtasks is named once, and a task inside the subtree is never
named, since it is deleted too. With `--promote-children` it covers the task
alone. Nothing is printed when no task depends on them. The delete itself is unaffected: the
dependency edges stay, and `bala task restore` brings them back.

Prints the id of each deleted task, one per line, on success.
With `--promote-children` the promoted subtasks are not deleted, so they are
not listed.

### `bala task restore`

Restores a previously deleted task.

```sh
bala task restore <task-id>
```

Prints the restored task, in the same format as `task ls`.

### `bala task complete`

Marks a task complete.

```sh
bala task complete <task-id> [--cascade]
```

| Flag | Description |
| --- | --- |
| `--cascade` | Also complete every incomplete descendant. |

Prints the completed task (and, with `--cascade`, each descendant it also
completed), one per line in the same format as `task ls`.

### `bala task reopen`

Reopens a previously completed task, marking it incomplete again.

```sh
bala task reopen <task-id>
```

Prints the reopened task, in the same format as `task ls`.

## `bala dep`

A dependency links a task to a *predecessor*: another task whose schedule
constrains it. A task may depend on any number of predecessors.

### `bala dep add`

Makes a task depend on a predecessor.

```sh
bala dep add <task-id> --on <predecessor-id> [--type fs|ss|ff|sf]
```

| Flag | Required | Description |
| --- | --- | --- |
| `--on` | yes | Id of the predecessor task. |
| `--type` | no | How the two tasks are linked. Defaults to `fs`. |

`--type` takes one of:

| Value | Meaning |
| --- | --- |
| `fs` | Finish-to-start (the default): the predecessor finishes before this task starts. |
| `ss` | Start-to-start: the predecessor starts before this task starts. |
| `ff` | Finish-to-finish: the predecessor finishes before this task finishes. |
| `sf` | Start-to-finish: the predecessor starts before this task finishes. |

Any other `--type` value is a usage error. Running `dep add` again on a pair
that is already linked replaces the dependency's type rather than adding a
second one; running it again with the same type changes nothing and still
prints the task.

Adding a dependency never moves either task's dates. If the dependent
task's dates already break the new dependency, it is still added, and the
task prints as ` (out of sync)` until its dates or its predecessor's are
changed to fit.

Prints the task in the same format as `task ls`, followed by one line per
predecessor it now depends on:

```text
  depends on: <predecessor-id> (<fs|ss|ff|sf>)
```

The command fails with a nonzero exit and an error message if:

- the task id or predecessor id doesn't exist (or names a deleted task);
- the task would depend on itself;
- the predecessor is one of the task's own ancestors or descendants;
- the new dependency would close a dependency cycle (the message names the
  tasks in the cycle).

### `bala dep rm`

Removes a task's dependency on a predecessor.

```sh
bala dep rm <task-id> --on <predecessor-id>
```

| Flag | Required | Description |
| --- | --- | --- |
| `--on` | yes | Id of the predecessor task. |

Prints the task and its remaining predecessors, in the same format as
`dep add`. Removing a dependency that doesn't exist is a no-op that still
prints the task. The predecessor may be a deleted task, so a dependency on
a task that has since been deleted can still be removed. Fails with a
nonzero exit if the task id doesn't exist or names a deleted task.

## `bala schedule`

Moves floating tasks to the dates their dependencies require, after showing
what would move and asking for confirmation.

```sh
bala schedule [--yes]
```

| Flag | Required | Description |
| --- | --- | --- |
| `--yes` | no | Apply the previewed moves without prompting. |

Only a *floating* task moves: one whose dates are not fixed (see `--float`
under [`bala task edit`](#bala-task-edit)). A task with fixed dates is never
changed, whatever its predecessors do, and neither is a completed one (a
completed task is never reported as out of sync either, though its dates
still decide where its successors go). Tasks
move predecessors first, so a chain of floating tasks is carried along in
one run.

A floating task that starts too early for a dependency moves later, just far
enough. Its due date follows: at start + duration if it has a duration,
otherwise keeping the length it had. A floating task with a duration also
has a missing date filled in, dependency or not: a start with no due date
gets `due = start + duration`, and a due date with no start gets
`start = due - duration`. A task with a duration and no dates at all waits
for a predecessor to give it one.

A start date that is set never moves earlier. A due date can: it follows the
duration, so a task whose duration is shorter than the span between its
dates comes out shorter when its start is moved.

The command first prints a preview, changing nothing:

```text
1 task(s) would move:
  <task-id> <title>: <start>..<due> -> <start>..<due>
1 task(s) would stay out of sync:
  <task-id> <title>
Apply? [y/N]
```

Each move line shows the task's current dates, then the dates it would move
to; an unset date prints as `?`, as in `task ls`. The "would stay out of
sync" section lists the tasks that would still break a dependency after the
moves (a fixed, incomplete task that starts before its predecessor finishes,
say) and
is printed only when there are any.

Answering `y` or `yes` applies the moves and prints each moved task, in the
same format as `task ls`. Any other answer prints `Aborted: nothing was
moved.` and changes nothing; the exit code is still zero. With `--yes` the
preview is still printed, then applied without the prompt.

When no task would move, the command prints `Nothing to move.` (followed by
the "would stay out of sync" section, if any task is out of sync), does not
prompt, and exits zero.

## `bala type`

Configures the list of task types available to `task add`/`task edit`'s
`--type` flag. A fresh database is seeded with one default type, key
`task`.

### `bala type ls`

Lists all configured task types.

```sh
bala type ls
```

Output is one type per line: `<key> <label> [color=<color>]
sort_order=<n>`. The `color=<color>` segment is only printed when the type
has a color set.

### `bala type set`

Creates a new task type, or updates an existing one by key.

```sh
bala type set <key> --label <label> [--color <color>] [--sort-order <n>]
```

| Flag | Required | Description |
| --- | --- | --- |
| `--label` | yes | The type's display label. |
| `--color` | no | A color for the type (any string; interpretation is left to consumers such as the TUI). |
| `--sort-order` | no | An integer used to order types relative to each other. |

When `<key>` already names an existing type, `label` is always overwritten
with the given value, but an omitted `--color`/`--sort-order` keeps that
type's current value rather than clearing it. When `<key>` is new,
an omitted `--color` leaves it unset and an omitted `--sort-order`
defaults to `0`.

Prints the resulting type, in the same one-line format as `type ls`.
