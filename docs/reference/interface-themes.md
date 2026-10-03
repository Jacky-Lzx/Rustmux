# Interface themes

```toml
[theme]
preset = "light" # "mocha" is the default

[theme.colors]
background = "#f5f6fa"
accent = "#18584a"
key = "#a32e77"
```

A session's theme colors its window bar, footer, pane frames and titles, History
footer, shortcut Help, editing/confirmation prompts and reload/save errors.
The session manager uses the configuration selected by its own client, including
its theme. Attaching with another `--config` does not replace the server's theme.
There is no process-global mutable palette; each render receives the applied
configuration's resolved colors.

`preset` accepts exactly `mocha` or `light`. Omitting `[theme]` or `preset` selects
Mocha. Optional color overrides inherit every unspecified value from the preset.
The palette and configuration syntax match the independent `main` track. Colors
must be strings of exactly `#RRGGBB`; hex digits may use either case. Unknown
presets, theme fields and color names, invalid value types, short hex, alpha hex
and named colors reject the entire configuration, including other settings.

| Color name | Interface role |
| --- | --- |
| `badge_text` | Text inside mode, window and shortcut label badges |
| `background` | Bar, footer, Help, prompt and manager backgrounds |
| `foreground` | Interface body text and inactive window badge backgrounds |
| `accent` | Active window badge, NORMAL mode and active pane frame/title |
| `muted` | Inactive pane frames/titles, manager metadata and Help footer text |
| `orange` | History and resize/move badges, pane History/activity frames, attached-session status |
| `error` | LOCKED badge and attached reload/save errors |
| `key` | Shortcut and prompt keys, Help title |
| `secondary` | Shortcut label badges, pane/tab/session badges, Help border/labels and prompt labels |
| `surface` | Help footer and manager divider |
| `surface_highlight` | Selected manager row |
| `border` | Manager outer frame |
| `blue` | Manager title, selected marker and editing text |
| `teal` | Manager session names |
| `warning` | Armed manager deletion confirmation |
| `purple` | Help group headings |

Mocha uses the existing Catppuccin palette. Manager frame/highlight/deletion and
Help headings now use their dedicated palette roles instead of sharing other
colors. Pane border backgrounds remain transparent/default.

Themes do not change child terminal cells, shell/application ANSI colors,
inherited terminal defaults, OSC color queries, cursor colors or History
selection highlights. A light interface does not force the child shell to use
a light background. Configure the outer terminal or application separately.

## Reload and inspection

The existing [configuration watcher](config-reload.md) checks every 500 ms.
A theme applies together with all other settings at the next safe input boundary.
Attached sessions defer changes until LOCKED mode and the end of Help, History,
editing, paste and input/drag sequences. Detached servers apply updates in their
server loop; reconnecting uses their latest applied theme. The manager updates
its own theme while open, including during search/name editing, while preserving
typed text.

Invalid updates retain the last good configuration and expose a reload error.
Removing `[theme]` from a valid file resets Mocha. Deleting a discovered default
file also restores defaults, subject to the existing restart-only binding rules.
Deleting an explicit `--config` file retains the last good configuration and
reports the missing file.

`default-config` includes `[theme]` with `preset = "mocha"`. `check-config --strict --toml` and `show-config -s NAME` report all 16 effective lowercase hex values in
`[settings.theme]`, including inherited values and overrides. This report is
resolved diagnostic output, not a configuration-file template.

## Review and verification

Review theme parsing and fallback colors in `src/theme.rs`, config/diagnostics
integration, then the applied runtime palette in `src/terminal.rs`. Rendering
uses it through `src/chrome.rs`, `src/pane_view.rs`, `src/prompt.rs`,
`src/shortcut_help.rs` and `src/session/picker.rs`.

Unit checks cover preset inheritance, all color overrides, malformed options,
local palette isolation, child cell preservation and themed Help/prompts/History
footers. CLI checks validate strict parsing and resolved diagnostics.
`tests/terminal_loop_themes.py` checks the actual binary with PTYs and sockets:
attached and detached theme changes, application ANSI colors, retained history
and child PIDs/variables, invalid updates rejected atomically, Help deferral,
History entry, reconnects, independent manager themes, manager reload during
search and discovered-config deletion.

This change awaits owner review. Installed-terminal visual inspection and Linux
CI have not been performed for this review branch.

Local cumulative verification on macOS, 2026-10-03:

- All-target tests: 957 passed, zero failed, six existing ignored tests, across
  47 test targets; all 40 real PTY scenarios passed.
- All-features Clippy with warnings denied, Rust formatting, `git diff --check`
  and the mdBook build passed.
