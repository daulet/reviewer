---
name: code-review
description: Review code changes using custom guidelines from ~/.config/reviewer/review_guide.md
---

# Code Review Skill

Review code changes (commits, branches, or staged files) following custom review guidelines.

## Review Guidelines

First, check for custom review guidelines at `~/.config/reviewer/review_guide.md`. If the file exists, load and follow those guidelines. If not, use these defaults:

- Look for bugs, logic errors, and edge cases
- Identify security vulnerabilities (injection, XSS, auth issues, etc.)
- Check for performance problems (N+1 queries, unnecessary allocations, etc.)
- Evaluate code clarity and maintainability
- Verify error handling is appropriate
- Check for missing tests for critical paths

## Workflow

1. **Determine what to review:**
   - If in a git worktree for a PR, review changes from the base branch
   - Otherwise, check for staged changes (`git diff --cached`)
   - If no staged changes, review uncommitted changes (`git diff`)
   - User can also specify a commit range or branch

2. **Gather context:**
   - Run `git diff` or `git log -p` to get the changes
   - Identify the files and languages involved
   - Read surrounding code for context when needed

3. **Perform the review:**
   - Analyze each changed file
   - Apply the review guidelines
   - Note specific line numbers for issues

4. **Provide structured feedback:**

## Output Format

Structure your review as follows:

### Summary
Brief overview of what the changes do and overall assessment (1-3 sentences).

### Issues Found
List issues by severity:

**Critical** (must fix before merge):
- `file.rs:42` - Description of critical issue

**Suggestions** (recommended improvements):
- `file.rs:78` - Description of suggestion

**Nitpicks** (minor style/preference):
- `file.rs:100` - Description of nitpick

### What Looks Good
Briefly note well-implemented aspects (helps authors know what to keep doing).

## Commands

Use these git commands to gather information:

```bash
# Get current branch info
git branch --show-current

# Check if in a worktree
git worktree list

# Get diff against main/master
git diff main...HEAD

# Get staged changes
git diff --cached

# Get unstaged changes
git diff

# Get specific commit
git show <commit>

# Get file at specific revision for context
git show HEAD~1:path/to/file.rs
```

## Example Review

### Summary
This PR adds user authentication with JWT tokens. The implementation is solid overall, but there's a potential security issue with token validation.

### Issues Found

**Critical:**
- `auth.rs:45` - Token expiration is not being checked. An expired token will still be accepted.

**Suggestions:**
- `auth.rs:23` - Consider using constant-time comparison for token validation to prevent timing attacks.
- `middleware.rs:67` - The error message reveals whether the user exists. Use a generic "invalid credentials" message instead.

**Nitpicks:**
- `auth.rs:12` - Unused import `std::collections::HashMap`

### What Looks Good
- Clean separation between authentication and authorization logic
- Good use of the Result type for error handling
- Comprehensive logging for auth failures (helpful for debugging)
