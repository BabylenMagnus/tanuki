# Fork provenance and upstream sync strategy

This repository is a fork of [Herdr](https://github.com/ogulcancelik/herdr) (Apache-2.0). See
`NOTICE` for the attribution statement and a summary of what was changed.

## Upstream fork point

- Upstream project: `github.com/ogulcancelik/herdr`
- Forked from tag `v0.7.5`, commit `99df3ac37be6bd7be2fd2023f0d88a7a0e7a7101`
- A read-only reference clone of upstream is kept at `Z:\Coding\Tanuki\sources\herdr` for diffing
  and picking up fixes; it is never modified or pushed to.
- This repo's own git history was squashed early on (see `chore: squash history, drop stale Herdr
  changelog content (v0.1.2)`), so upstream lineage is **not** recoverable from `git log`/`git
  blame` here — this file is the source of truth for the fork point instead of a tag or merge
  base.

## What diverges from upstream

Full detail lives in `NOTICE`. Short version: identifiers, CLI binary name, environment variable
prefixes, socket/file names, and user-facing strings were rebranded from `herdr`/`Herdr` to
`tanuki`/`Tanuki`; a cloud transport layer was added for the Tanuki platform; the `opencode` agent
is white-labeled as "Tanuki" in the sidebar/toasts (`display_agent_label` in
`src/detect/mod.rs`) without changing its underlying `opencode` identity used for config/manifest
lookups.

## Syncing future upstream changes

There is no automated upstream-tracking branch or remote configured in this repo (deliberately —
the rebrand touches identifiers broadly enough that a raw `git merge`/`git rebase` against
upstream would conflict pervasively). To pull in an upstream fix or feature:

1. Check `Z:\Coding\Tanuki\sources\herdr` for the change (`git log`/`git diff` against the commit
   or tag noted above, or `git pull` there first to see what's new upstream — never push from
   that clone).
2. Read the relevant upstream commit(s) and port the logic manually into this repo, translating
   identifiers as needed (`herdr` → `tanuki`, `Herdr` → `Tanuki`, `HERDR_*` env vars → `TANUKI_*`,
   etc. — see the "Rebrand find-replace bugs" lesson in the wiki page below before doing a
   mechanical sweep).
3. Run `just check` (or `just windows-test` on Windows) before committing the ported change.
4. Note the upstream commit/PR reference in the commit body (`refs upstream <sha>` or similar) so
   future syncs can tell what's already been ported.

The vendored `libghostty-vt` dependency has its own, separate sync process — see
`vendor/libghostty-vt.vendor.json` and `vendor/libghostty-vt.patches.md`, documented in
`AGENTS.md`. That process is unrelated to syncing the rest of upstream Herdr.

For the full rebrand history, naming timeline, and known rebrand pitfalls, see
`Z:\Coding\Tanuki\tanuki-wiki\wiki\tanuki-terminal-overview.md` in the parent monorepo's wiki.
