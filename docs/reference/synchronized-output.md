# Synchronized Output

CSI ? 2026 h begins a synchronized update; CSI ? 2026 l ends it. While enabled,
the CLI keeps the last displayed frame while continuing to parse child output
into the model. After the mode is disabled, it paints the latest model using the
normal frame schedule. See the [protocol description](https://contour-terminal.org/vt-extensions/synchronized-output/).
DECRQM reports 1 while enabled and 2 otherwise.

## Bounded waiting

Rustmux releases an observed batch after one second, clears mode 2026 and resumes
normal painting. Repeated enable commands do not extend that observed deadline.
The event loop checks at most 50 ms apart when idle; scheduling and blocked output
can add delay. One second is Rustmux policy, not a protocol-mandated timeout.
A disable/re-enable wholly inside one parsed chunk can share the earlier deadline;
this conservative bound avoids needing a queue of batch transitions.

RIS and DECSTR clear the mode. The CLI also ends a batch on valid outer resize
or PTY EOF so dimensions and final output can be shown. The model's standalone
resize operation preserves the mode; terminating a batch is event-loop policy.
Cursor saves and alternate-screen switches preserve it.

Input, signal handling and query replies continue while painting is paused.
The model and existing bounded queues hold state; no output-history buffer is
added. An already queued frame is completed before reading more child output.
Intermediate completed batches may still be coalesced by the normal frame
schedule. Input-mode changes that are emitted by rendering are delayed too.

This batches the model locally. It does not forward mode 2026 to the outer
terminal or guarantee atomic physical display of the resulting ANSI frame.
Standalone render/Renderer calls paint the supplied model immediately; the CLI
owns the pause and timeout. Exit cleanup uses the normal terminal restoration.

## Verification

`cargo test --test synchronized_output` checks parsing boundaries, queries,
state preservation and resets. Event-loop unit tests use explicit instants to
check timeout boundaries, repeated enable, explicit end and EOF.
The real PTY suite checks suppression of intermediate text, replies while
paused, explicit completion, stalled-batch timeout, resize, EOF and SIGTERM.
