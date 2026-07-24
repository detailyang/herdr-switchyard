# Switchyard

Switchyard is a project and coding session picker for [Herdr](https://herdr.dev/).
Press one key, choose a configured project and session, and Switchyard focuses the
running agent or opens its session directory as a Herdr workspace and resumes it.

## Model

- A **project** is a configured Git repository and its default Herdr agent.
- A **worktree session** is an isolated persistent Git worktree and its agent
  context. It starts at the configured base in detached HEAD state.
- A **local session** runs directly in the configured project directory. One
  project can have multiple local sessions; they share a Herdr workspace but use
  separate agent tabs and native agent contexts.
- Project configuration and the session registry are separate files managed through
  Herdr's plugin config and state directories.

## Install

Switchyard requires Herdr 0.7.5 or newer and a Rust toolchain for installation:

```sh
herdr plugin install detailyang/herdr-switchyard
```

Install the Herdr integration for each agent you use so Switchyard can retain native
session identities:

```sh
herdr integration install codex
herdr integration install claude
herdr integration install pi
```

Add a shortcut to `~/.config/herdr/config.toml`:

```toml
[[keys.command]]
key = "prefix+g"
type = "plugin_action"
command = "herdr.switchyard.open"
description = "open Switchyard"
```

Apply the keybinding with `herdr server reload-config`, then press `prefix+g`.

## Use

The project screen displays only explicitly configured projects. Choose **Add
project** or press `a` to open the directory picker. Choose a directory and
**Add this folder**. Switchyard derives the project name from the directory and
uses `pi`, an automatically detected base branch, and worktree sessions by default.
It prefers `origin/HEAD`, then an active or existing `main`/`master`, then another
usable local branch. If the directory is not a Git repository, Switchyard honors
Git's `init.defaultBranch` setting and otherwise uses `main`, then creates an empty
initial commit so it can create worktrees immediately. Existing project entries
with a missing base branch are repaired automatically; valid explicit overrides
remain unchanged.

Use the arrow keys or mouse wheel to move, `Enter` or a double-click to open a
directory, and start typing to filter the current directory list; the first match
is selected for `Enter`. `Backspace` edits a non-empty filter, then moves to the
parent when the filter is empty. Hidden directories are omitted by default; click
**Show hidden** or press `.` to toggle them. `Esc` clears a non-empty filter, then
closes the picker.

Open a project and choose **New session** or press `n`. The title identifies the
session; it is not a Git branch name. Choose **Worktree** (the default) for an
isolated detached worktree, or **Local** to run directly in the project directory.
Use the left/right arrow keys or click a mode. Multiple local sessions are allowed
for the same project and each receives its own agent tab.

The picker supports both keyboard and mouse input. Click a project or session to
select it, double-click a session to open it, use the mouse wheel to move through
either list, and click **Add project** or **New session** to run that action.
Right-click a project or session for a small menu that can pin it to the top of
its list or remove it. Press `Delete` or `Backspace` on a selected project or
session to open the same removal confirmation directly. Removing a project only
unregisters it from Switchyard and requires its sessions to be removed first;
the project directory is never deleted. Removing a worktree session deletes its
worktree and registry entry; dirty worktrees and detached commits that are not
reachable from a Git ref are refused rather than discarded. Removing a local
session only removes its registry entry and never deletes project files. Close the
session's Herdr workspace (worktree) or exact agent tab (local) before removing it.

Worktree placement inherits Herdr's `worktrees.directory` setting. Switchyard does
not keep a second worktree-root setting that could disagree with Herdr.

Selecting a stored session follows this order:

1. focus its running agent if the workspace is already open;
2. create a dedicated session-root tab and restart the Agent if it is not running;
3. open the registered worktree or project directory and resume the agent if it is
   dormant;
4. report `missing` without destructive repair if a worktree path disappeared.

Codex and Claude use their native session id when a Herdr integration reported one;
Pi uses its recorded session path. Without one, Codex and Claude fall back to their
native resume picker, while Pi starts a new session so a new directory does not
open an empty resume picker.

## Configuration

Ask Herdr for the plugin config directory; the project file is `config.toml` inside
it:

```sh
herdr plugin config-dir herdr.switchyard
```

The file is TOML and may also be edited manually:

```toml
version = 1

[ui]
theme = "jade-dark"

[[projects]]
id = "ai-infra"
name = "AI Infra"
path = "/Users/me/work/ai-infra"
agent = "pi"
base_branch = "main"
agent_args = []
```

Built-in themes are `jade-dark` (default), `midnight-dark`, `paper-light`, and
`sand-light`. Change `[ui].theme` and reopen Switchyard to apply it. Unknown theme
names are rejected instead of silently falling back.

Switchyard writes `sessions.json` under `HERDR_PLUGIN_STATE_DIR`. It records only
Switchyard-managed sessions, their local/worktree mode, paths, timestamps, and the
most recent native agent session reference. Herdr remains the runtime source for
workspace, pane, and agent status.

## Develop

```sh
cargo build --release
herdr plugin link "$PWD"
```

`plugin link` does not run build commands, so rebuild the binary after source
changes. Run the test suite with:

```sh
cargo test
```
