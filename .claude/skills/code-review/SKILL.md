---
name: code-review
description: Interactive code review with GitHub comment submission using gh CLI
---

# Code Review Skill

Interactively review code changes and submit approved comments to GitHub using `gh` CLI.

## Review Guidelines

First, check for custom review guidelines at `~/.config/reviewer/review_guide.md`. If the file exists, load and follow those guidelines. If not, use these defaults:

- Look for bugs, logic errors, and edge cases
- Identify security vulnerabilities (injection, XSS, auth issues, etc.)
- Check for performance problems (N+1 queries, unnecessary allocations, etc.)
- Evaluate code clarity and maintainability
- Verify error handling is appropriate
- Check for missing tests for critical paths

## Workflow

### Phase 1: Gather Information

1. **Determine the PR context:**
   ```bash
   # Get repo info
   gh repo view --json nameWithOwner --jq '.nameWithOwner'

   # List open PRs to find the one we're reviewing
   gh pr list --json number,headRefName,title

   # Get current branch
   git branch --show-current
   ```

2. **Get the diff:**
   ```bash
   # Get diff with context (10 lines)
   git diff -U10 main...HEAD
   ```

3. **Read surrounding code** for additional context when needed.

### Phase 2: Analyze and Identify Issues

Analyze the diff and categorize issues:

- **Critical**: Must fix before merge (bugs, security issues)
- **Suggestions**: Recommended improvements
- **Nitpicks**: Minor style/preference issues

Keep track of:
- File path
- Line number (from the NEW version, lines with `+`)
- Issue description
- Severity level

### Phase 3: Interactive Review

Present each issue ONE AT A TIME to the user for approval.

For each issue, show:
```
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Issue 1 of N [SEVERITY]

File: path/to/file.rs
Line: 42

Comment:
  [Your detailed comment about the issue]

Context:
  [Show the relevant code snippet]

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

Then ask: **"Submit this comment? (y)es / (s)kip / (e)dit / (q)uit review"**

- **yes**: Submit the comment using `gh` CLI
- **skip**: Skip this comment, move to next
- **edit**: Let user modify the comment text, then ask again
- **quit**: Stop the review process

### Phase 4: Submit Comments

When user approves a comment, submit it using:

```bash
# For line-specific comments on a PR
gh api repos/{owner}/{repo}/pulls/{pr_number}/comments \
  -X POST \
  -f body="[Comment text]" \
  -f commit_id="$(git rev-parse HEAD)" \
  -f path="[file path]" \
  -f line=[line number] \
  -f side="RIGHT"
```

If line-level comment fails, fall back to a general PR comment:
```bash
gh pr comment {pr_number} --body "[File:Line] [Comment text]"
```

### Phase 5: Final Summary and Approval

After reviewing all issues, show a summary:

```
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Review Complete

Comments submitted: X
Comments skipped: Y
Critical issues: Z

[If critical issues > 0]
  ⚠️  Critical issues were found. PR should not be approved until addressed.

[If critical issues == 0]
  ✓ No critical issues found.
  Would you like to approve this PR? (y)es / (n)o
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

If user chooses to approve:
```bash
gh pr review {pr_number} --approve --body "Looks good! [optional summary]"
```

## Important Notes

- Always confirm the PR number before submitting any comments
- Use `gh auth status` to verify authentication if commands fail
- Line numbers must correspond to the NEW version of the file (right side of diff)
- Be constructive and specific in comments
- Explain *why* something is an issue, not just *what*
