# Contributing

Use Node.js 24 and enable the repository-local commit rules after cloning:

```sh
node scripts/commit/install.mjs
```

This installs a `commit-msg` hook and commit template through local Git config.
It is idempotent and refuses to replace existing custom hook/template settings;
when using other hook tooling, invoke `node scripts/commit/check.mjs --message-file "$1"`
from your existing commit-msg hook instead. Node must be available to the Git client.

Every commit, including merges, reverts and internal maintenance, needs exactly
one `Release Notes:` section outside code fences. Example for internal work:

```text
chore: update CI tooling

Release Notes:
- N/A
```

For user-facing changes, use the English bullets documented in
[scripts/release/README.md](scripts/release/README.md). Empty sections, placeholder
bullets, duplicate sections and mixing N/A with actual notes are rejected.
The template intentionally contains a placeholder instead of silently choosing N/A.
Preserve the notes in GitHub squash/merge messages as well.

CI checks every commit in the push before..after or PR base..head range, using
the PR head rather than GitHub's synthetic test merge. Existing history is not
rewritten or retroactively checked. Manual runs and new-branch pushes check the
current tip. Missing history fails validation. This adds no job or network request.
Local hooks can be bypassed, so require the existing **CI result** status in branch
protection to prevent merging commits that fail the rule. The repository workflow
does not change GitHub branch protection or block pushes at the server.

## Compatibility code

New or modified code retained for an older API, format, behavior, environment,
or caller must have a local `COMPAT(<stable-reason>)` comment at each independent
compatibility branch or adapter. A note elsewhere in the repository is not a
substitute. Reuse the same reason identifier for related compatibility paths.

Each comment must identify:

- The older interface or behavior being supported.
- Why the compatibility code is still required.
- The replacement that new callers should use.
- The concrete condition for removing the compatibility code, including a
  removal version or date only when one has been agreed.

If no removal version is planned, explicitly say so; for example: `No removal
version has been scheduled.` This does not replace the removal condition. Compatibility
support alone does not mean an API is deprecated: do not claim imminent removal
or add deprecation attributes without an agreed deprecation plan.

For example:

```text
// COMPAT(legacy-widget-api):
// Supports: widgets created through the previous initialization API.
// Reason: existing callers have not all migrated.
// Replacement: use the current initialization API.
// Removal: remove after support for the previous API ends.
// No removal version has been scheduled.
```

When changing an existing compatibility path, reassess its removal condition.
If it is met, remove the compatibility code or explain why it must remain.
When removing compatibility code, update its compatibility-only tests,
documentation, and related markers in the same change. Preserve tests for
behavior that remains supported.

Ownership checks, lifecycle validation, and recovery required by the current
contract are not automatically compatibility code. Reviewers must check for
missing annotations as well as incomplete ones; a marker format check alone
cannot reliably detect an unannotated compatibility path.
