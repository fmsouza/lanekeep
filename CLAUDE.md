@AGENTS.md

<!--
Everything about working in this repository lives in AGENTS.md, imported above.
Keep this file to Claude-specific mechanics only. Anything a different agent would
also need belongs in AGENTS.md, or the two will disagree and the disagreement will
be discovered by whichever one is wrong at the time.
-->

## Claude-specific notes

- `.claude/settings.json` formats Rust files on write. If a `cargo fmt` failure appears
  after an edit, the edit produced code rustfmt cannot parse — fix the syntax rather than
  reaching for the hook.
- `just check` is the gate. Prefer it over running `cargo` subcommands directly, so what
  you see matches what CI sees.
- When a change touches the sandbox boundary, the cache key, or anything in the
  invariants section of AGENTS.md, say so explicitly in the pull request body. Those are
  the changes worth a second look.
