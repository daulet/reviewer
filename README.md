# reviewer

Automate mundane parts of code review. Speed up your workflow with LLM.

## Features

- List PRs needing your attention across multiple repositories
- Approve PRs or add comments without leaving the terminal
- Add line-level comments directly from the diff view
- Enhanced diff rendering with [delta](https://github.com/dandavison/delta) (side-by-side, syntax highlighting)
- Launch [Claude Code](https://github.com/anthropics/claude-code) for AI-assisted reviews
- Continuously learn from your feedback to improve AI accuracy

## Installation

### Homebrew (macOS)

```bash
brew tap daulet/tap
brew install reviewer
```

### From releases

Download the latest binary from [Releases](https://github.com/daulet/reviewer/releases).

### From source

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
| [Claude Code](https://github.com/anthropics/claude-code) | AI-assisted code reviews | `npm install -g @anthropic-ai/claude-code` |

The diff view automatically detects if delta is installed and uses it for rendering. Otherwise, falls back to built-in syntax highlighting.

## Usage

```bash
reviewer                       # Scan configured directory
reviewer -m                    # Show my PRs (instead of PRs to review)
reviewer -r ~/dev              # Specify repos directory
reviewer -d                    # Include draft PRs
reviewer -e archived -e old    # Exclude directories
reviewer -e vendor --save-exclude  # Save exclusions to config
```

On first run, you'll be prompted to set your repos root directory.

### Keybindings

**List view:**
| Key | Action |
|-----|--------|
| `j/k` | Navigate |
| `Ctrl+d/u` | Page down/up |
| `g/G` | First/last |
| `Enter` | Open PR details |
| `/` | Search PRs |
| `n/N` | Next/previous search match |
| `o` | Open in browser |
| `y` | Copy PR URL |
| `R` | Refresh |
| `d` | Toggle drafts |
| `q` | Quit |

**Detail view:**
| Key | Action |
|-----|--------|
| `Tab` | Switch tabs (Description/Diff/Comments) |
| `j/k` | Scroll |
| `Ctrl+d/u` | Page down/up |
| `/` | Search in diff |
| `:` | Go to line number |
| `n/N` | Next/previous search match |
| `c` | Add line comment (in Diff tab) |
| `r` | Launch Claude review |
| `a` | Approve |
| `x` | Close PR with comment |
| `m` | Merge PR (squash, `--my` mode only) |
| `o` | Open in browser |
| `y` | Copy PR URL |
| `p` | Previous PR |
| `q` | Back to list |

## Claude Code Integration

For AI-assisted reviews, set up the code-review skill:

### 1. Install the skill

```bash
mkdir -p ~/.claude/skills/code-review
cp .claude/skills/code-review/SKILL.md ~/.claude/skills/code-review/
```

### 2. Configure Claude Code permissions

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

## Configuration

Config is stored at:
- macOS/Linux: `~/.config/reviewer/config.json`
- Windows: `%APPDATA%\reviewer\config.json`

```json
{
  "repos_root": "/path/to/your/repos",
  "exclude": ["archived", "vendor"]
}
```

## License

MIT
