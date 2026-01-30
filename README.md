# reviewer

Automate mundane parts of code review. Speed up your workflow with LLM.

## Features

- List PRs needing your attention;
- Approve PRs or add comments without leaving the terminal;
- Launch [Claude Code](https://github.com/anthropics/claude-code) for AI-assisted reviews;
- Continuously learn from your own feedback to improve AI accuracy on next review;

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

## Requirements

- [GitHub CLI](https://cli.github.com/) (`gh`) - authenticated with `gh auth login`
- [Claude Code](https://github.com/anthropics/claude-code) (optional) - for AI-assisted reviews

## Usage

```bash
reviewer                       # Scan configured directory
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
| `a` | Approve PR |
| `R` | Refresh |
| `d` | Toggle drafts |
| `q` | Quit |

**Detail view:**
| Key | Action |
|-----|--------|
| `Tab` | Switch tabs (Description/Diff/Comments) |
| `j/k` | Scroll |
| `c` | Add line comment (in Diff tab) |
| `r` | Launch Claude review |
| `a` | Approve |
| `n/p` | Next/previous PR |
| `q` | Back to list |

## Claude Code Setup

For AI-assisted reviews, install the code-review skill:

```bash
mkdir -p ~/.claude/skills/code-review
cp .claude/skills/code-review/SKILL.md ~/.claude/skills/code-review/
```

If running Claude sandboxed, add these permissions to `~/.claude/settings.json`:

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

## License

MIT
