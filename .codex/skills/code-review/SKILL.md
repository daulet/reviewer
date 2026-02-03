---
name: code-review
description: Interactive code review with GitHub comment submission using gh CLI
---

# Code Review Skill

Interactively review code changes and submit approved comments to GitHub using `gh` CLI.

## Review Guidelines

First, check for custom review guidelines at `~/.config/reviewer/review_guide.md`. If the file exists:
1. Follow any custom review focus areas
2. **Check the "Skip These" section** - do NOT flag issues matching skip categories

If no custom guidelines exist, use these defaults:

- Look for bugs, logic errors, and edge cases
- Identify security vulnerabilities (injection, XSS, auth issues, etc.)
- Check for performance problems (N+1 queries, unnecessary allocations, etc.)
- Evaluate code clarity and maintainability
- Verify error handling is appropriate
- Check for missing tests for critical paths

## Workflow

### Phase 1: Gather Information

**IMPORTANT:** You are already in a git worktree with the PR branch checked out. The code is local - use git directly, NOT `gh` commands to fetch diffs.

1. **Get the PR base commit and diff:**
   ```bash
   # Get the PR's base branch and find the merge-base (where PR diverged)
   PR_NUMBER=<from initial prompt>
   BASE_SHA=$(gh pr view $PR_NUMBER --json baseRefOid --jq '.baseRefOid')

   # Diff from the merge-base to HEAD (the PR's changes only)
   git diff -U10 $BASE_SHA...HEAD
   ```

   If `gh pr view` fails, fall back to merge-base with origin/main:
   ```bash
   MERGE_BASE=$(git merge-base origin/main HEAD)
   git diff -U10 $MERGE_BASE HEAD
   ```

2. **Get PR metadata** (for comment submission):
   ```bash
   # Get repo info
   gh repo view --json nameWithOwner --jq '.nameWithOwner'

   # The PR number is provided in the initial prompt
   ```

3. **Read surrounding code** using local files for additional context when needed.

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
# For line-specific comments, use a review with comments array                                                                          
  gh api repos/{owner}/{repo}/pulls/{pr_number}/reviews \
    -X POST \
    -f event="COMMENT" \
    -f body="" \
    --raw-field comments='[{"path":"[file path]","line":[line number],"body":"[Comment text]"}]'
```

Or for a single inline comment without creating a full review:                                                                                             

```bash
# Use the correct parameters for single-line comments                                                                                   
  gh api repos/{owner}/{repo}/pulls/{pr_number}/comments \
    -X POST \
    -f body="[Comment text]" \
    -f commit_id="$(git rev-parse HEAD)" \
    -f path="[file path]" \
    -F line=[line number] \
    -f side="RIGHT"
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
# Approve WITHOUT --body to avoid duplicate comments
gh pr review {pr_number} --approve
```

**DO NOT use `--body` with approve** - comments are submitted separately in Phase 5. Adding `--body` creates a duplicate.

### Phase 7: Learn from Skipped Comments

After the review ends (approved or not), if any comments were skipped, offer to update the user's review guidelines to avoid similar suggestions in future reviews.

1. **Identify skipped comments**: Track which issues the user chose NOT to submit.

2. **Extract categories**: For each skipped comment, determine its abstract category. Don't use the specific code or text - extract the general pattern.

   Examples of category extraction:
   - Skipped: "Unused import `os`" → Category: "unused imports"
   - Skipped: "Consider using `const` instead of `let`" → Category: "const vs let preferences"
   - Skipped: "Add JSDoc comment for this function" → Category: "missing documentation comments"
   - Skipped: "Line exceeds 80 characters" → Category: "line length warnings"
   - Skipped: "Use early return pattern" → Category: "early return style suggestions"

3. **Present categories to user**:
   ```
   ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
   Learn from skipped comments?

   You skipped these types of feedback:
     1. unused imports
     2. missing documentation comments

   Add to your review guidelines to skip in future? (y)es / (n)o
   ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
   ```

4. **Update guidelines file**: If user confirms, append to `~/.config/reviewer/review_guide.md`:

   ```bash
   # Create file if it doesn't exist
   mkdir -p ~/.config/reviewer

   # Append skip rules
   cat >> ~/.config/reviewer/review_guide.md << 'EOF'

   ## Skip These
   - unused imports
   - missing documentation comments
   EOF
   ```

   If the file already has a "## Skip These" section, append only the new categories under it (avoid duplicates).

5. **Confirm update**:
   ```
   ✓ Updated ~/.config/reviewer/review_guide.md
     Added 2 categories to skip list.
   ```

**Important**:
- Only offer this if there were skipped comments
- Extract abstract categories, not specific instances
- Deduplicate categories (if user skipped 3 "unused import" issues, that's one category)
- Check existing skip rules to avoid duplicates

## Important Notes

- **NO `--body` on approve** - comments are submitted separately in Phase 5
- Always confirm the PR number before submitting any comments
- Use `gh auth status` to verify authentication if commands fail
- Line numbers must correspond to the NEW version of the file (right side of diff)
- Be constructive and specific in comments
- Explain *why* something is an issue, not just *what*
- Review guidelines at `~/.config/reviewer/review_guide.md` - check "Skip These" section before flagging issues
