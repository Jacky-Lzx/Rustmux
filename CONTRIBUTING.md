# Human Review Requirements

`main` is the former `main-human`; `main-AI` is the former `main`. The branch
rename preserves the existing contribution scope and review requirements.

**Current contribution scope:** `main` only accepts PRs that fix bugs in
its existing code and issues reporting those bugs. Feature implementation PRs
and feature-related issues are not currently accepted on this track; target
`main-AI` instead, including for features available there but not on `main`.

This scope restriction does not change the owner's ability to implement features
through personal direct commits.

The owner may personally commit directly to `main` without a PR or a
separate PR review record, and is responsible for reviewing those changes before
committing. This exception does not apply to AI actions using the owner's Git
identity. AI and other contributors need a PR and the owner's personal review
of the final commit.

The target branch determines policy. Existing `track:main-human` and `track:main`
labels use the former names; do not infer a label rename from the branch rename.
AI may design, implement, test and suggest review findings. AI must not write
owner confirmation, infer approval from authorship or successful CI, or mark a
feature accepted without the owner's explicit acceptance record.

Each PR includes the linked issue, behavior and acceptance criteria, approach
and intentional differences from the fixed main reference, reading order,
verification results and limitations, final SHA, and owner's review record.
If the owner chooses to use a PR, their personal decision to merge the reviewed
final revision needs no separate self-review record. AI and other contributor
PRs require the owner's explicit final-revision review. Changes after review
require review of the updated final commit before merging.

Use non-closing issue references until the owner confirms acceptance. Merge and
feature acceptance are distinct: a partial implementation does not close its
issue. Closing as obsolete or not planned also requires owner confirmation and
does not increase accepted coverage.

For owner direct commits, link the commit SHA instead of a PR in the acceptance
record. Direct submission does not automatically accept a feature or close an
issue; verification and the owner's explicit acceptance remain required.

The historical ledger and progress page remain on `main-AI`. Only count functionality
that is present on `main`, verified against its criteria and personally
accepted.

These are repository policies, not an assertion that GitHub protection rules or
labels are configured. Configure those separately when publishing this track.

## Local commit checks

Run `./scripts/install-git-hooks.sh` once in any repository worktree. The
installed pre-commit hook checks `cargo fmt` and runs Clippy with warnings
denied when staged files affect Rust code or its build configuration. Other
commits skip these Rust checks. When rustfmt finds changes, it formats the
working tree and blocks that commit so the result can be reviewed and staged
explicitly. CI still runs the full Rust checks for every change.

For a short edit cycle, run `cargo test --lib --locked` for model changes or
`cargo test --test <target> --locked` for the affected integration test. For
example, `cargo test --test terminal_loop graphics --locked` runs only the
graphics PTY scenarios; `cargo test --test terminal_loop -- --list` shows the
other scenario names. Before review, run
`cargo test --all-targets --locked -- --test-threads=4`, matching CI's bounded
parallel PTY coverage.
