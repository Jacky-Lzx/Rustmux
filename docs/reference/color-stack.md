# Kitty Color Stack

A child can temporarily change its color profile and restore the preceding one
using the [Kitty color-stack protocol](https://sw.kovidgoyal.net/kitty/color-stack/):

| Command | Operation |
| --- | --- |
| `OSC 30001 ST` | Push the current pane color profile |
| `OSC 30101 ST` | Pop and restore its most recently saved profile |

Both ST (`ESC` followed by a backslash) and BEL termination work across input
chunk boundaries. These commands have no parameters or replies. Noncanonical
codes, arguments, cancellation and unfinished or oversized strings do not apply
an operation. Query restored values with the existing OSC 4 and OSC 10/11/12
[palette and default-color queries](status-replies.md).

For example, inside a pane:

```sh
printf '\033]30001\033\\\033]11;#203040\033\\'
# Run the application or commands using the temporary background.
printf '\033]30101\033\\'
```

## State and isolation

A saved profile contains the pane's foreground, background, cursor and all 256
palette overrides. Unset values retain their meaning of inheriting the outer
terminal's current profile. A pop restores that distinction rather than freezing
previously inherited RGB values. For example, if a pane saved an inherited
background and then reconnects to a client with a different background, popping
restores inheritance of the new background. Explicit colors restore their saved
RGB values; OSC 104/110/111/112 resets still resume current inheritance.

Each pane owns a separate stack shared by its primary and alternate grids.
Push before entering a full-screen application; pop in either grid to restore
its profile. Switching focus or windows does not consume or replace saved
profiles. Resize, primary reflow and reconnect retain the stack without restarting
the child. Pop on an empty stack does nothing.

SGR, cursor save/restore and DECSTR leave the stack intact. RIS discards saved
profiles, while retaining the existing behavior of pane-local color overrides;
a subsequent pop cannot resurrect a pre-reset profile. Respawn starts a new
screen with an empty stack. Saved workspace/history files contain no color-stack
commands or profiles; restoration starts fresh processes and terminal state.

Existing cells keep their symbolic style attributes. Restoring colors recolors
pane content on the next composed frame, including default/indexed foreground,
background and indexed underline colors. The active pane supplies the rendered
cursor color. Rustmux interface colors retain their independent theme, and the
push/pop commands are never forwarded to the outer terminal. Rendering and
History snapshots can share immutable saved profiles without consuming live
stack entries.

The stack retains at most 32 profiles. Pushing at capacity discards the oldest
entry before adding the new one. This bounds memory while preserving the most
recent nested changes. Profiles are shared across screen clones, avoiding a
complete palette copy for every render snapshot. The parser's existing 64-byte
OSC limit and per-byte reply bound are unchanged.

## Review and verification

Based on reviewed hyperlink commit `6385135`, with `main` reference fixed at
`57d598657ad7acf00d6a0ddf734fba8f48d50e4c`. The reference's `TerminalOscTracker`
implements the same OSC push/pop commands and 32-entry retention. This increment
uses the human track's `Screen`, palette inheritance and existing renderer.
Intentional differences include preserving unset inheritance bindings, clearing
the stack on RIS and rejecting arguments rather than ignoring them.

Reading order: profile state and push/pop in `src/screen.rs`, OSC dispatch in
`src/parser.rs`, then resize/reset lifecycle and `tests/color_stack.rs`.
Integration tests cover every palette entry, nested restoration, overflow and
underflow, both terminators at every split, malformed input, SGR/cursor state,
buffer switches, resets, screen cloning, resize/reflow and existing-cell redraw.
Unit tests check current outer-profile inheritance and snapshot omission.

`tests/terminal_loop_color_stack.py` runs two real probes through a named server.
It checks replies with and without an attached client, background-pane isolation,
active cursor synchronization, buffer changes, soft/hard reset, History/help,
resize, three attachments with different outer profiles, and unchanged child
PIDs. It also verifies that stack controls never reach the outer stream.

Local cumulative validation on Rust 1.99.0 passed 1,005 tests across 50 targets,
with 6 pre-existing ignored tests. All 44 real-PTY scenarios passed. All-target,
all-feature Clippy with warnings denied, formatting, `git diff --check` and the
mdBook build passed.

This increment covers the existing foreground/background/cursor/ANSI profile.
OSC 21 structured color control, selection-specific colors and XTerm
XTPUSHCOLORS/XTPOPCOLORS/XTREPORTCOLORS aliases remain outside its scope. The shared
acceptance ledger is unchanged; the new feature awaits owner review.

The branch has not been pushed. Linux CI and actual GUI color appearance have not
been verified.
