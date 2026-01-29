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

### Phase 3: Present All Issues At Once

Show ALL issues in a summary table first:

```
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
 #  │ Severity │ Location              │ Issue Summary
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
 1  │ CRITICAL │ auth.rs:45            │ Token expiration not checked
 2  │ SUGGEST  │ auth.rs:23            │ Use constant-time comparison
 3  │ SUGGEST  │ middleware.rs:67      │ Error message reveals user existence
 4  │ NITPICK  │ auth.rs:12            │ Unused import
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

Then show FULL details of each issue below the table:

```
──────────────────────────────────────────────────────
[1] CRITICAL - auth.rs:45
──────────────────────────────────────────────────────
Token expiration is not being checked. An expired token will still be
accepted, allowing unauthorized access.

Context:
    43│     let token = extract_token(&headers)?;
    44│     let claims = decode_token(&token)?;
  > 45│     Ok(claims.user_id)  // Missing: check claims.exp
    46│ }
──────────────────────────────────────────────────────

[2] SUGGEST - auth.rs:23
...
```

### Phase 4: Batch Selection

After showing all issues, prompt for batch action:

```
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Which comments to submit?

  a = all          Submit all comments
  c = critical     Submit only CRITICAL issues
  n = none         Skip all, proceed to summary
  1,2,4 or 1-3     Select specific numbers
  q = quit         Cancel review

Your choice:
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

Parse user input:
- `a` or `all`: Submit every comment
- `c` or `critical`: Submit only CRITICAL severity
- `n` or `none`: Don't submit any, proceed to summary
- Numbers like `1,2,4` or `1-3,5`: Submit selected issues
- `q` or `quit`: Cancel the entire review

### Phase 5: Submit Selected Comments

For each selected comment, submit using `gh` CLI:

```bash
# For line-specific comments on a PR (note: line must be integer with -F, not -f)
gh api repos/{owner}/{repo}/pulls/{pr_number}/comments \
  -X POST \
  -f body="[Comment text]" \
  -f commit_id="$(git rev-parse HEAD)" \
  -f path="[file path]" \
  -F line=[line number] \
  -f side="RIGHT" \
  -f subject_type="line"
```

**Important API notes:**
- Use `-F line=123` (not `-f`) so the number is sent as integer, not string
- `subject_type` must be `"line"` for single-line comments
- `side` should be `"RIGHT"` for new code (lines with `+`)
- `commit_id` must be the HEAD commit of the PR branch

If line-level comment fails, fall back to a general PR comment:
```bash
gh pr comment {pr_number} --body "**[file:line]** [Comment text]"
```

Show progress as comments are submitted:
```
Submitting comments...
  [1] auth.rs:45 ✓
  [2] auth.rs:23 ✓
  [3] middleware.rs:67 ✗ (failed - added as general comment)
Done. 3 comments submitted.
```

### Phase 6: Final Summary and Approval

Show final summary:

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
