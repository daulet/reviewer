# TODO

## Features

- [x] Update skill to update skill when user chooses "skip" to enhance review for the next time
- [x] **Line-level review comments** - Allow attaching comments to specific lines in the diff view
  - Track cursor position in diff
  - Capture file path + line number
  - Use GitHub review comments API: `POST /repos/{owner}/{repo}/pulls/{pr}/comments`
  - Required fields: `path`, `line`, `commit_id`, `body`
- [ ] add age to PRs?
- [ ] Better diff view, research better algos, check the stars
- [ ] Add an option to close PRs with comment
- [ ] Review my PRs, ie address comments
- [ ] Capture PRs approved, but review re-requested
- [ ] Search option by id or substring in list view

## Bugs
- [x] Add an option to exclude dirs, def exclude worktrees
- [ ] Double PR approve, the PR in question received the approve comment twice.
