# TODO

## Features

- [ ] **Line-level review comments** - Allow attaching comments to specific lines in the diff view
  - Track cursor position in diff
  - Capture file path + line number
  - Use GitHub review comments API: `POST /repos/{owner}/{repo}/pulls/{pr}/comments`
  - Required fields: `path`, `line`, `commit_id`, `body`
