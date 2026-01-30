# TODO

## Features

- [ ] Add view refresh option in top list view
- [ ] Filter out draft PRs, have an option to include them
- [x] Update skill to update skill when user chooses "skip" to enhance review for the next time
- [ ] **Line-level review comments** - Allow attaching comments to specific lines in the diff view
  - Track cursor position in diff
  - Capture file path + line number
  - Use GitHub review comments API: `POST /repos/{owner}/{repo}/pulls/{pr}/comments`
  - Required fields: `path`, `line`, `commit_id`, `body`
- [ ] Update readme with install instructions eg installing skill under `~/.claude/skills/code-review/`
- [ ] Review my PRs, ie address comments 

## Bugs
- [ ] Double PR approve, the PR in question received the approve comment twice.
