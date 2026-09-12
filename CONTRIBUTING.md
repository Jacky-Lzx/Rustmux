# Human Review Requirements

**Current contribution scope:** `main-human` only accepts PRs that fix bugs in
its existing code and issues reporting those bugs. Feature implementation PRs
and feature-related issues are not currently accepted on this track; target
`main` and use `track:main` instead, including for features already available on
`main` but not yet implemented on `main-human`.

This scope restriction does not change the owner's ability to implement features
through personal direct commits.

The owner may personally commit directly to `main-human` without a PR or a
separate PR review record, and is responsible for reviewing those changes before
committing. This exception does not apply to AI actions using the owner's Git
identity. AI and other contributors need a PR and the owner's personal review
of the final commit.

Use `track:main-human` for eligible bug reports and bug-fix PRs targeting
`main-human`. The target branch determines policy.
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

The shared ledger and progress page live on `main`. Only count functionality
that is present on `main-human`, verified against its criteria and personally
accepted.

These are repository policies, not an assertion that GitHub protection rules or
labels are configured. Configure those separately when publishing this track.
