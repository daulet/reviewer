# reviewer

How do you keep up with AI generated PRs? Answer: use AI assisted code reviews.

![alt text](./docs/image.png)
## Features

- List, comment, approve PRs from your cloned repos;
- Start interactive AI assisted review session, let it learn your code review approach;  
- Run reviewer as daemon to kick off reviews as they are created;

## Installation

Homebrew (macOS):
```bash
brew tap daulet/tap
brew install reviewer
```

From source:
```bash
cargo install --git https://github.com/daulet/reviewer
```

## Setup

### Required

- [GitHub CLI](https://cli.github.com/) (`gh`) - authenticated with `gh auth login`

### Optional

| Tool | Purpose | Install |
|------|---------|---------|
| [delta](https://github.com/dandavison/delta) | Enhanced diff rendering (side-by-side, syntax highlighting) | `brew install git-delta` |
| [Codex CLI](https://github.com/openai/codex) | AI-assisted code reviews (OpenAI) | `npm install -g @openai/codex` |
| [Claude Code](https://github.com/anthropics/claude-code) | AI-assisted code reviews | `npm install -g @anthropic-ai/claude-code` |

The diff tries to use `delta` if installed. Choice of code reviewer tool can be configured in `~/.config/reviewer/config.json`.

## AI Code Review Setup

For AI-assisted reviews, set up a code-review skill and pick a provider.

### 1. Install the skill

We keep a single skill definition under `.claude/skills/code-review/SKILL.md` and reuse it
for both providers. This repo includes `.codex/skills/code-review/SKILL.md`, so Codex will
pick it up when you run Codex from the repo root. If you prefer a global install, copy it
into `~/.codex/skills`:

```bash
mkdir -p ~/.claude/skills/code-review
cp .claude/skills/code-review/SKILL.md ~/.claude/skills/code-review/

mkdir -p ~/.codex/skills/code-review
cp .claude/skills/code-review/SKILL.md ~/.codex/skills/code-review/
```

Restart Codex after adding skills. In a Codex session, run `/skills` to confirm the
`code-review` skill is available, then invoke it with `$code-review`.
Note: Reviewer launches the AI tool from the PR worktree, so repo-local `.codex/skills`
in this repo won't be visible to Codex unless you install the skill globally.

### 2. Configure provider permissions (optional)

**Claude Code**

Add to `~/.claude/settings.json`:

```json
{
  "permissions": {
    "allow": [
      "Read(path:~/.config/reviewer/**)"
    ]
  },
  "sandbox": {
    "excludedCommands": ["gh"]
  }
}
```

**Codex CLI**

This repo includes `.codex/config.toml` and `.codex/rules/reviewer.rules` with conservative
defaults and allowlists for common review commands. If you want global defaults, mirror
them in `~/.codex/config.toml` and `~/.codex/rules`.

```
prefix_rule(
  pattern = ["gh", "pr", ["view", "comment", "review"]],
  decision = "allow",
)

prefix_rule(
  pattern = ["gh", "api"],
  decision = "allow",
)

prefix_rule(
  pattern = ["gh", "repo", "view"],
  decision = "allow",
)

prefix_rule(
  pattern = ["git", ["diff", "merge-base", "rev-parse"]],
  decision = "allow",
)
```

### 3. Customize review guidelines (optional)

Create `~/.config/reviewer/review_guide.md` to customize what the AI looks for:

```markdown
## Focus Areas
- Security vulnerabilities
- Performance issues
- Error handling

## Skip These
- Unused imports
- Minor style issues
```

The AI learns from skipped comments and offers to update this file automatically.

## Usage

```bash
reviewer                       # Scan configured directory
reviewer -d                    # Include draft PRs
reviewer -r ~/dev              # Specify repos directory
reviewer -e archived -e old    # Exclude directories

reviewer --my                  # Show PRs you authored (same as -m)

reviewer daemon init           # Pick repos to monitor
reviewer daemon run            # Start daemon polling loop
reviewer daemon status         # Show daemon state/counters
```

On first run, you'll be prompted to set your repos root directory.

Use `--my` (or `-m`) to switch to "my PRs" mode. In this mode, reviewer
shows PRs authored by your GitHub account and enables `m` in detail view to
merge mergeable PRs with squash.

Daemon notes:
- On first daemon setup, reviewer shows an interactive checkbox list of repos and saves exclusions by `owner/repo`.
- In daemon init UI, press `f` on a selected repo to open a subdirectory tree popup.
  Use `j/k` (or arrows) to move, `Enter` to expand/collapse, and `Space` to mark paths.
- Existing open PRs are seeded as already seen during init, so only newly opened PRs trigger.
- PR updates do not retrigger review; tracking is persisted in `~/.config/reviewer/daemon_state.json`.
- Optional `daemon.repo_subpath_filters` lets you restrict a repo to PRs touching specific subpaths.
  Omit a repo (or set an empty list) to monitor all PRs in that repo.

## Terminal Launch Harness (macOS)

Testing-only utility to validate terminal launch behavior (tab/window/session execution).

```bash
reviewer harness --terminal-app Ghostty --terminal-launch-mode same-space --runs 3
reviewer harness --terminal-app Ghostty --terminal-launch-mode new-tab --runs 3
```

## Configuration

Config is stored at:
- macOS/Linux: `~/.config/reviewer/config.json`
- Windows: `%APPDATA%\reviewer\config.json`

AI settings are optional. `prompt_template` supports `{pr_number}`, `{repo}`, `{title}`,
`{review_guide}`, and `{skill}` placeholders.
On macOS/Linux, `terminal_app` lets you pick which terminal launches AI reviews (default: Terminal on macOS).
On macOS, optional `terminal_launch_mode` values are:
- `auto`, `new-instance`, `same-space`, `new-tab`, `new-window`
Note: Ghostty `same-space`/`new-tab` use `System Events` keystroke automation and may require macOS Accessibility/Automation permissions.
Daemon state is stored separately in:
- macOS/Linux: `~/.config/reviewer/daemon_state.json`
- Windows: `%APPDATA%\reviewer\daemon_state.json`

```json
{
  "repos_root": "/path/to/your/repos",
  "exclude": ["archived", "vendor"],
  "daemon": {
    "poll_interval_sec": 60,
    "exclude_repos": ["org/legacy-repo"],
    "repo_subpath_filters": {
      "org/monorepo": ["services/payments", "infra/terraform"],
      "org/full-repo": []
    },
    "initialized": true,
    "include_drafts": false
  },
  "ai": {
    "provider": "codex",
    "command": "codex",
    "args": [],
    "skill": "code-review",
    "prompt_template": "Review PR #{pr_number} in {repo}. Title: \"{title}\". Use {skill}. Follow {review_guide}",
    "terminal_app": "Ghostty",
    "terminal_launch_mode": "new-tab"
  }
}
```

## License

MIT
