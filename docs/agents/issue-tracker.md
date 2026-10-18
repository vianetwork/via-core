# Issue tracker: GitHub, with local markdown for private detail

## Choosing the tracker

- Default: GitHub issues in `vianetwork/via-core`. This repository is public.
- Keep private infrastructure detail in a local file under `.scratch/issues/`: host and database names, credentials, and live incident data. A GitHub issue can describe the same defect without them.
- A wayfinder map and all its child tickets live in one tracker. The operator chooses the tracker when the map is created.
- `.scratch/` is git-ignored and is not a backup. `git clean -x` deletes it.

## Issue forms

`.github/ISSUE_TEMPLATE/` holds YAML issue forms. `gh issue create` cannot fill a form. Use the form's title format, use each field `label` as a body heading, and add the form's labels.

- L1, Bitcoin watch or sender, reorg detector, verifier sync, or external-node bootstrap: `l1_btc_reorg_sync_bug.yml`.
- Runtime, protocol, database, deployment, or operator-observed correctness: `operator_evidence_report.yml`.
- Other user-facing bugs: `bug_report.yml`.
- Security issues: a private security advisory, never a public issue.
- Wayfinder maps, wayfinder tickets, and `/to-tickets` work slices are not bug reports. Create them as plain issues.

## GitHub conventions

- **Create an issue**: `gh issue create --title "..." --body "..."`. Use a heredoc or `--body-file` for multi-line bodies.
- **Read an issue**: `gh issue view <number> --json number,title,body,labels,comments`.
- **List issues**: `gh issue list --state open --json number,title,body,labels,comments --jq '[.[] | {number, title, body, labels: [.labels[].name], comments: [.comments[].body]}]'` with the needed `--label` and `--state` filters.
- **Make an issue a sub-issue of a parent**: `gh issue create --parent <parent> ...`, or `gh issue edit <parent> --add-sub-issue <child>` afterwards.
- **Comment on an issue**: `gh issue comment <number> --body "..."`
- **Apply or remove labels**: `gh issue edit <number> --add-label "..."` / `--remove-label "..."`
- **Close**: `gh issue close <number> --comment "..."`

`gh` infers the repo from `git remote -v` when it runs inside this clone.

## Pull requests as a triage surface

**PRs as a request surface: no.** _(Set to `yes` if this repo treats external PRs as feature requests; `/triage` reads this flag.)_

When set to `yes`, PRs run through the same labels and states as issues, using the `gh pr` equivalents:

- **Read a PR**: `gh pr view <number> --comments` and `gh pr diff <number>` for the diff.
- **List external PRs for triage**: `gh api --paginate 'repos/{owner}/{repo}/pulls?state=open' --jq '.[] | select(.author_association | IN("OWNER","MEMBER","COLLABORATOR") | not) | {number, title, author: .user.login, author_association, labels: [.labels[].name]}'`.
- **Comment, label, close**: `gh pr comment`, `gh pr edit --add-label`/`--remove-label`, `gh pr close`.

GitHub shares one number space across issues and PRs, so a bare `#42` may be either. Resolve it with `gh pr view 42` and fall back to `gh issue view 42`.

## When a skill says "publish to the issue tracker"

Create a GitHub issue. For private detail, create a file under `.scratch/issues/` instead.

## When a skill says "fetch the relevant ticket"

Read it as in **Read an issue** above, or read the `.scratch/` file at the given path.

## Wayfinding operations

Used by `/wayfinder`. The **map** is a single issue with **child** issues as tickets.

- **Map**: a single issue labelled `wayfinder:map`, holding the Notes / Decisions-so-far / Fog body. `gh issue create --label wayfinder:map`.
- **Child ticket**: an issue linked to the map as a GitHub sub-issue (see **Make an issue a sub-issue of a parent**). Labels: `wayfinder:<type>` (`research`/`prototype`/`grilling`/`task`). Once claimed, the ticket is assigned to the driving developer.
- **Blocking**: GitHub's native issue dependencies. Add an edge with `gh api --method POST repos/<owner>/<repo>/issues/<child>/dependencies/blocked_by -F issue_id=<blocker-db-id>`, where `<blocker-db-id>` is the blocker's numeric database id (`gh api repos/<owner>/<repo>/issues/<n> --jq .id`), not the `#number` or `node_id`. GitHub reports `issue_dependencies_summary.blocked_by` (open blockers only). A ticket is unblocked when every blocker is closed.
- **Frontier query**: list the map's open children, drop any with an open blocker or an assignee. The first in map order wins.
- **Claim**: `gh issue edit <n> --add-assignee @me`, the session's first write.
- **Resolve**: `gh issue comment <n> --body "<answer>"`, then `gh issue close <n>`, then append a context pointer (gist + link) to the map's Decisions-so-far.
