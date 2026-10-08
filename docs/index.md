<section class="landing-hero">
  <div class="hero-copy">
    <p class="hero-kicker">Fast, persistent, terminal-native</p>
    <h1 class="hero-title">
      <img class="hero-brand-icon" src="theme/rustmux-icon.svg" alt="" aria-hidden="true">
      <span class="hero-wordmark">
        <span class="hero-wordmark-name">Rus<span class="hero-gradient-letter">t</span><span class="hero-wordmark-accent">mux</span></span>
        <span class="hero-wordmark-tagline">TERMINAL MULTIPLEXER</span>
      </span>
    </h1>
    <p class="hero-summary">Yet another terminal multiplexer, a spiritual successor to <a href="https://zellij.dev/">Zellij</a>.</p>
    <div class="hero-actions">
      <a class="primary-button" href="getting-started/quick-start.html">Get started <span aria-hidden="true">→</span></a>
      <a class="secondary-button" href="https://github.com/Jacky-Lzx/Rustmux">View on GitHub</a>
    </div>
    <div class="hero-meta" aria-label="Project highlights">
      <span>Kitty-native</span>
      <span>macOS + Linux</span>
      <span>Written in Rust</span>
    </div>
  </div>
  <div class="terminal-window" aria-label="Illustration of a Rustmux workspace">
    <div class="terminal-chrome">
      <i class="terminal-dot"></i><i class="terminal-dot"></i><i class="terminal-dot"></i>
      <span class="terminal-label">Rustmux — work</span>
    </div>
    <div class="mux-preview">
      <div class="mux-window-bar">
        <span class="mux-session">Rustmux <span>(work)</span></span>
        <span class="mux-tab">1 editor</span>
        <span class="mux-tab active">2 server</span>
        <span class="mux-tab">3 notes</span>
      </div>
      <div class="mux-panes">
        <section class="mux-pane active">
          <span class="mux-pane-title">~/projects/rustmux</span>
          <div class="mux-command"><span>❯</span> cargo test</div>
          <div class="mux-output"><strong>running tests</strong><br><span class="mux-success">✓</span> test result: ok<br><span class="mux-muted">workspace ready</span></div>
          <div class="mux-command"><span>❯</span></div>
        </section>
        <section class="mux-pane">
          <span class="mux-pane-title">yazi</span>
          <div class="mux-command"><span>❯</span> yazi</div>
          <ul class="mux-files">
            <li class="selected"><b>›</b> src/</li>
            <li><b>›</b> docs/</li>
            <li><b>·</b> Cargo.toml</li>
            <li><b>·</b> README.md</li>
          </ul>
        </section>
      </div>
      <div class="mux-status-bar">
        <span class="mux-key">Ctrl b</span>
        <span class="mux-mode">NORMAL</span>
        <span class="mux-key">?</span>
        <span class="mux-help">HELP</span>
      </div>
    </div>
  </div>
</section>

> [!WARNING]
> **Rustmux is still at an early stage. Configuration and persistence formats may evolve.**

## TL;DR

- Zellij-inspired terminal multiplexing.
- Persistent sessions, flexible panes.
- Kitty-native, built with Yazi in mind.
- Mostly AI-generated, with an ongoing effort to better understand and improve the code.

This documentation describes `main`, formerly named `main-human`. The older,
independent implementation remains on `main-AI`. Read
[Branches and Compatibility](reference/documentation-status.md) for command,
configuration and storage differences, and
[Development and Contributions](reference/development.md) for the review policy.

Start an unnamed foreground workspace with `rustmux`, or create a persistent
workspace with `rustmux new work`. Use `rustmux attach work --create` to create,
restore or reconnect to a named workspace. The terminal panel above is an
illustration, not a benchmark or a test report.

<div class="hero-actions">
  <a class="primary-button" href="getting-started/quick-start.html">Get started <span aria-hidden="true">→</span></a>
</div>

## A Few Thoughts

I have used [tmux](https://github.com/tmux/tmux) for years.
A terminal without a multiplexer feels like a desk with nowhere to leave an unfinished page.
I want to step away and return to find my work where I left it: the editor open, a process still running, a few panes holding the threads of an idea.
tmux has the bearing of a swordsman from an older age: spare, disciplined, and dependable.
There is a quiet dignity to a tool that serves you for years without asking for much attention.
But the terminal around it has kept growing, and so has my sense of what working in one could feel like.

[Zellij](https://zellij.dev/) gives that feeling a shape.
There is care in making a workspace approachable, in helping someone discover an action, in making panes and sessions feel like things they can comfortably arrange and inhabit.
Rustmux owes much of its sensibility to that care.
Calling it a spiritual successor is a statement of affection and intent; Zellij remains very much its own living project.
What I want to carry forward is the belief that a terminal workspace deserves thoughtful design, down to the small interactions we repeat all day.

[Kitty](https://sw.kovidgoyal.net/kitty/) opens another door.
Richer keyboard input, graphics, and closer integration with the desktop expand what an application can express inside a terminal.
These capabilities make me reluctant to accept that adding a multiplexer should mean giving some of them up.
Every layer between an application and the terminal takes on a responsibility: to preserve as much of that conversation as it can.

[Yazi](https://yazi-rs.github.io/) makes that responsibility concrete.
A file manager brings you into contact with the ordinary texture of your work: directories, images, previews, files to move and open.
Here, a missing capability becomes a small interruption you feel in your hands.
A preview should appear where you expect it.
A new pane should begin in a directory that makes sense.
Enough care with details like these can make a collection of separate programs feel like a place you know your way around.

Rustmux grows out of wanting these things together: the continuity I have long relied on, the consideration I admire in Zellij, and room for applications to use what Kitty makes possible.
Yazi helps keep that ambition grounded in everyday use.
The aim is a workspace that remembers enough, responds readily, and asks for attention only when there is something worth attending to.

AI helped turn building a terminal multiplexer from a distant ambition into something I could sit down and work on, one evening at a time.
It gave the project its first momentum, helping me get the basic functionality working quickly enough that an idea became something I could actually use, question, and improve.
There is something deeply encouraging about that moment: a tool you had only imagined begins to respond beneath your hands, and the distance between wanting to make it and being able to begin suddenly feels smaller.

But I want my part in this project to amount to more than passing requests to AI and accepting whatever comes back.
The early foundations came together quickly with its help; I aim to deepen my understanding of the code through ongoing review and improvement.
That means taking the time to trace a behavior, question a decision, and learn enough to recognize when a plausible solution is the wrong one.
I want to be able to explain why the code is there, what it promises, and where it might fail.
AI made it easier to begin. Making this a project I understand and can stand behind is the work I am choosing to do.

This is still a small, unfinished project.
There are rough edges, and much of the work ahead lives in details that will never make an impressive screenshot.
I think those details are worth the effort.
When I sit down at a terminal, I want to pick up a thought where I left it. Rustmux is my attempt to make that a little easier.

<p style="text-align: right;">
  — Jacky Li<br>
  <em style="color: #888;">This section was generated by AI. Sorry, writing isn't my strong suit.</em>
</p>
