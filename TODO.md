# TODO

## Features

- [x] Add view refresh option in top list view (R key)
- [x] Filter out draft PRs, have an option to include them (d key to toggle)
- [x] Update skill to update skill when user chooses "skip" to enhance review for the next time
- [ ] **Line-level review comments** - Allow attaching comments to specific lines in the diff view
  - Track cursor position in diff
  - Capture file path + line number
  - Use GitHub review comments API: `POST /repos/{owner}/{repo}/pulls/{pr}/comments`
  - Required fields: `path`, `line`, `commit_id`, `body`
- [ ] Update readme with install instructions eg installing skill under `~/.claude/skills/code-review/`
- [ ] Review my PRs, ie address comments 

## Bugs
- [ ] Proper linter
- [ ] Double PR approve, the PR in question received the approve comment twice.
