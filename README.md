<p align="center">
  <img src="docs/theme/rustmux-icon.svg" alt="Rustmux logo" width="128" height="128">
</p>

<h1 align="center">Rustmux</h1>

<p align="center">
  <strong>A modern terminal workspace, built in Rust.</strong><br>
  Persistent sessions · Flexible panes · Kitty-native protocols
</p>

<p align="center">
  <a href="https://jacky-lzx.github.io/Rustmux/">Documentation</a> ·
  <a href="#quick-start">Quick start</a> ·
  <a href="#see-it-in-action">Demos</a> ·
  <a href="#development-tracks">Development tracks</a> ·
  <a href="CONTRIBUTING.md">Contributing</a>
</p>

<p align="center">
  <a href="https://github.com/Jacky-Lzx/Rustmux/actions/workflows/ci.yml"><img src="https://github.com/Jacky-Lzx/Rustmux/actions/workflows/ci.yml/badge.svg?branch=main" alt="CI status"></a>
  <a href="https://github.com/Jacky-Lzx/Rustmux/actions/workflows/docs.yml"><img src="https://github.com/Jacky-Lzx/Rustmux/actions/workflows/docs.yml/badge.svg?branch=main" alt="Documentation deployment status"></a>
</p>

Rustmux brings together the continuity of persistent terminal sessions, a
[Zellij](https://zellij.dev/)-inspired modal interface, and modern terminal
protocols for applications such as Kitty and Yazi. Leave a workspace running,
return to it later, and keep your shells, editors, and tools close at hand.

**macOS and Linux · Stable Rust toolchain**

> [!WARNING]
> Rustmux is still at an early stage. Configuration and persistence formats may
> evolve. Windows is not currently supported.

## Quick start

```sh
git clone --branch main https://github.com/Jacky-Lzx/Rustmux.git
cd Rustmux
cargo run --release --locked
```

To install the binary from the checkout:

```sh
cargo install --path . --locked
rustmux
```

Running `rustmux` without arguments opens an **unnamed foreground workspace**.
It starts in **locked** mode, where input goes directly to your shell. To create
and later reconnect to a persistent named session:

```sh
rustmux new work
# Detach with Ctrl-b, then d; reconnect with:
rustmux attach work
# Create or restore the workspace if its server is no longer running:
rustmux attach work --create
```

The default shortcuts on `main` are:

| First steps | Keys |
| --- | --- |
| Enter normal mode | <kbd>Ctrl-b</kbd> |
| Open contextual help | <kbd>Ctrl-b</kbd> → <kbd>?</kbd> |
| Create a window | <kbd>Ctrl-b</kbd> → <kbd>c</kbd> |
| Split right / down | <kbd>Ctrl-b</kbd> → <kbd>%</kbd> / <kbd>"</kbd> |
| Rename a window | <kbd>Ctrl-b</kbd> → <kbd>,</kbd> |
| Browse history | <kbd>Ctrl-b</kbd> → <kbd>[</kbd> |
| Open Session Manager in a named session | <kbd>Ctrl-b</kbd> → <kbd>Ctrl-w</kbd> |
| Detach a named session and leave programs running | <kbd>Ctrl-b</kbd> → <kbd>d</kbd> |

Unnamed tabs follow the foreground program by default: `fish`, then `yazi` while
Yazi is open, then `fish` after it exits. Explicit names remain fixed. Set
`tab_name = "title"` to follow terminal titles instead; see
[Automatic and Explicit Names](docs/reference/windows.md#automatic-and-explicit-names).

See [Installation and Quick Start](docs/getting-started/quick-start.md) for shell
selection and configuration setup. Use `--config PATH` to select a configuration,
`rustmux config default` to inspect built-in settings, and
`rustmux config check --strict` to detect unsupported options. Keybindings and
configuration spellings differ between `main` and `main-AI`.

## See it in action

These recordings were made on `main-AI`. They illustrate the general workflows;
use the shortcuts above and the linked documentation for the current `main`
implementation. Appearance, shortcuts and configuration can differ.

**Arrange your workspace.** Create and rename windows, split panes, rearrange
layouts, and zoom the active pane.

![Creating windows, splitting panes, and rearranging a Rustmux workspace](https://raw.githubusercontent.com/Jacky-Lzx/Rustmux/57d598657ad7acf00d6a0ddf734fba8f48d50e4c/demos/assets/windows-and-panes.gif)

<details>
<summary><strong>Session Manager — browse, search, and switch</strong></summary>

Browse running and saved sessions. Press `/` to search, then Enter to open a
match, or press `a` to enter a new session name.

![Browsing and searching sessions in the Rustmux Session Manager](https://raw.githubusercontent.com/Jacky-Lzx/Rustmux/57d598657ad7acf00d6a0ddf734fba8f48d50e4c/demos/assets/session-manager.gif)

</details>

<details>
<summary><strong>History and help — find output and discover shortcuts</strong></summary>

Search scrollback, copy selections, and discover actions in the contextual help
overlay. Help shortcuts can also be executed directly from the overlay.

![History search and contextual keybinding help in Rustmux](https://raw.githubusercontent.com/Jacky-Lzx/Rustmux/57d598657ad7acf00d6a0ddf734fba8f48d50e4c/demos/assets/history-and-help.gif)

</details>

The recording instructions and VHS tapes remain on
[`main-AI`](https://github.com/Jacky-Lzx/Rustmux/blob/main-AI/demos/README.md).

## Built for everyday terminal work

| Capability                     | What it gives you                                                                                                                                                                                                |
| ------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Persistent sessions**        | Detach without stopping programs. Save layouts, working directories, and optional scrollback for later restoration. [Sessions →](docs/reference/session-cli.md)                                                         |
| **Flexible panes**             | Split, resize, move, zoom, and use floating terminals. Move panes across windows while preserving their running processes. [Windows and panes →](docs/reference/windows.md)                                    |
| **History tools**              | Search output, copy with OSC 52, and open full history or the previous command's output in your editor. [History and copy →](docs/reference/history-view.md)                                                              |
| **Kitty and Yazi integration** | Graphics, extended keyboard input, file drag and drop, rich clipboard, file transfer, and notifications. Availability depends on the outer terminal and enabled policies. [Compatibility →](docs/reference/input-loop.md#current-compatibility) |
| **A configurable interface**   | Modal shortcuts, themes, live configuration reloads, and attention indicators for panes that need you. [Configuration →](docs/reference/config-diagnostics.md)                                                            |
| **Scriptable workspaces**      | Start project layouts, send input to specific panes, capture output, and save sessions from scripts. [Automation →](docs/reference/script-control.md)                                                                    |

Each pane maintains its own terminal state, and rendering updates changed cells.
Detaching preserves running programs; restoring a disk snapshot starts new
processes with the saved layout, directories and optional history. Autosave and
saved scrollback are opt-in; manual saving remains available. See
[Session Snapshots](docs/reference/session-snapshots.md).

Graphics require verified outer-terminal support and exact cell pixels. Child
clipboard access, file transfer and drag/drop use separate opt-in settings.
Protocol support is bounded by the documented subset and does not guarantee
compatibility with every application. Use `cargo compat` for the installed
applications' optional checks; these are separate from the normal test suite.

## Development tracks

AI helped turn Rustmux from an idea into a usable tool. The project keeps room
for that exploration while building a version developed through the owner's
personal review and ongoing learning.

| | `main` (formerly `main-human`) | `main-AI` (formerly `main`) |
| --- | --- | --- |
| Focus | Implementation under personal review | Rapid exploration and feature development |
| Code | Human-written or AI-generated | Human-written or AI-generated |
| PR review | Must be performed by the project owner | May be performed by AI |
| Issue acceptance and closure | Require the project owner's confirmation | May be performed by AI |

The two branches have independent implementations and histories. **Human review
is the requirement on `main`; human-only authorship is not.** Current `main`
includes split and floating panes, persistent sessions, saved workspaces, History,
script control and the documented terminal protocols.

The contribution scope on `main` currently accepts bug reports and bug-fix PRs;
feature implementation PRs and feature-related issues target `main-AI`. Read
[Contributing](CONTRIBUTING.md) for the full policy. Existing issue labels may
still use the former branch names.

The [historical implementation plan and acceptance ledger](https://github.com/Jacky-Lzx/Rustmux/blob/main-AI/docs/reference/human-review-plan.md)
remain on `main-AI`. Their dated snapshots are not the current `main` feature
status. Configuration and saved-session formats are not interchangeable between
the branches; see [Branches and Compatibility](docs/reference/documentation-status.md).

## Find your way around

| Start here                                           | Go further                                                       |
| ---------------------------------------------------- | ---------------------------------------------------------------- |
| [Build and run](docs/getting-started/quick-start.md)    | [Project layouts](docs/reference/project-layouts.md)   |
| [Keybindings and modes](docs/reference/windows.md#interactive-controls)   | [Kitty and Yazi](docs/reference/input-loop.md#current-compatibility)                       |
| [Configuration](docs/reference/config-diagnostics.md)         | [Themes](docs/reference/interface-themes.md)                           |
| [Branches and Compatibility](docs/reference/documentation-status.md) | [Development, tests, and fuzzing](docs/reference/development.md) |

Read the [online documentation](https://jacky-lzx.github.io/Rustmux/) or browse
its [source in this repository](docs/index.md). Before opening an issue or PR,
read [Contributing](CONTRIBUTING.md) and choose the appropriate development track.

<details>
<summary><strong>Build the documentation locally</strong></summary>

With [mdBook](https://rust-lang.github.io/mdBook/) installed:

```sh
mdbook build
mdbook serve --open
```

Use mdBook 0.5.4, matching the documentation workflow. Build output is written to
`dist/`; the preview rebuilds as you edit. GitHub Pages publishes the documentation
from `main`.

</details>
