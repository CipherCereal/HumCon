---
description: Verify a component meets the done bar before moving on
command: /done-check
---

Review the code just written against this checklist:

1. **Compiles/runs with no ignored warnings** — build succeeds, no `#[allow(...)]` bandaids for new code
2. **Handles obvious edge cases** — empty state, missing file, permission denial, malformed input
3. **Was actually run with real data for a few minutes** — not just once, exercised multiple paths
4. **architecture.md updated** — documents what it does, known limitations, and TODOs

Report pass/fail on each point plainly. Do not mark done if any point fails.
