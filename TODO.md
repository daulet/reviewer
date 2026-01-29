# TODO

## Features

- [ ] Double PR approve, the PR in question received the approve comment twice.
- [x] Still pane clearing issues while launching claude and hitting tabs (fixed with terminal.clear())
- [ ] Update skill to update skill when user chooses "skip" to enhance review for the next time
- [ ] **Line-level review comments** - Allow attaching comments to specific lines in the diff view
  - Track cursor position in diff
  - Capture file path + line number
  - Use GitHub review comments API: `POST /repos/{owner}/{repo}/pulls/{pr}/comments`
  - Required fields: `path`, `line`, `commit_id`, `body`
- [ ] Add view refresh option in top list view
- [ ] Update readme with install instructions eg installing skill under `~/.claude/skills/code-review/`
