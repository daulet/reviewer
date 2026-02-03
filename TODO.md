# TODO

## Features

- [x] Open in default terminal, not "terminal"
- [ ] Improve codex code review skill with interactive prompts like in claude 
- [ ] Render description in markdown
- [ ] Still not working: update skill to update skill when user chooses "skip" to enhance review for the next time
- [x] add age to PRs?
- [x] Add an option to close PRs with comment
- [x] Review my PRs, ie address comments
- [ ] Capture PRs approved, but review re-requested
- [x] Search option by id or substring in list view
- [ ] Request changes (review action alongside approve)
- [x] Open in browser (`o` key)
- [x] Show CI/status checks
- [ ] Reply to comments / resolve threads
- [x] Visualize diff context around comments (not just list)
- [x] Copy PR URL to clipboard (`y` key)
- [ ] Checkout PR locally for testing
- [ ] Re-request review (--my mode)
- [ ] Per-file navigation in large diffs
- [ ] Filter by repo/author/label
- [ ] Address comments in my PRs via LLM

## Bugs
- [ ] when using builtin diff there are artifacts after scrolling past long lines
- [x] Add an option to exclude dirs, def exclude worktrees
- [ ] Repo might not be checked out for PRs found via search, so add cloning when necessary
- [ ] Double PR approve, the PR in question received the approve comment twice.
