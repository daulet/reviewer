# TODO

## Features

- [ ] Update skill to update skill when user chooses "skip" to enhance review for the next time
- [ ] Add an option to jump pages
- [ ] **Line-level review comments** - Allow attaching comments to specific lines in the diff view
  - Track cursor position in diff
  - Capture file path + line number
  - Use GitHub review comments API: `POST /repos/{owner}/{repo}/pulls/{pr}/comments`
  - Required fields: `path`, `line`, `commit_id`, `body`
- [ ] Review my PRs, ie address comments 

## Bugs
- [ ] Where do we config the root, looks like its implied
- [ ] Add an option to exclude dirs, def exclude worktrees
- [x] Proper linter
- [ ] Double PR approve, the PR in question received the approve comment twice.
