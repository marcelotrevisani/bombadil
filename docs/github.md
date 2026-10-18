# GitHub pull requests

When a project is on a branch that isn't the repository's default branch, and
that branch has an open pull request on GitHub, the **GitHub** tab shows the
pull request and every check run on its latest commit.

## What you see

- The pull request's number, title, and whether it is a draft, with
  **Open pull request**.
- A summary: checks still running, all passed, or how many failed, the
  commit, and when Bombadil last looked.
- One row per check, failures first, with its workflow, how long it took,
  and **Open** to go to the run on GitHub.

When there is nothing to show, the tab says why: not a git repository, no
GitHub remote, on the default branch, no open pull request, or not signed in.

## How often

Bombadil looks every two minutes, for every project, and once at launch.
**Refresh** looks at the selected project immediately.

## Notifications

You are notified, on the desktop and inside Bombadil, when a pull request's
checks start and when they finish, with how many passed or failed. Nothing is
announced for the first look after Bombadil starts or after you switch
branches, so opening the app is quiet.

## Signing in

Bombadil uses the [GitHub CLI](https://cli.github.com/) when it is installed
and signed in (`gh auth login`). That covers GitHub Enterprise hosts too.

When `gh` is signed in to more than one github.com account, pick the one
Bombadil should use under **Settings › GitHub pull requests › Account**. Until
you pick one, Bombadil uses whichever account `gh` has active. Your choice
applies to Bombadil only: `gh` in your terminal keeps its own active account.
If the chosen account later signs out of `gh`, the tab says so rather than
switching to another account.

**Add account** signs another account in without a terminal. Bombadil shows a
one-time code. Enter it at [github.com/login/device](https://github.com/login/device),
using **Copy code** and **Open GitHub**. Once you approve, the new account is
selected, and `gh` switches back to whichever account it had active before.

Without `gh`, **Sign in with browser** does the same without a terminal:
enter the one-time code at [github.com/login/device](https://github.com/login/device)
and approve Bombadil. GitHub asks for the `repo` scope, which private
repositories need; Bombadil only reads with it. **Remove** signs out.

Or add a personal access token in **Settings › GitHub pull
requests**. A fine-grained token with read-only access to *Pull requests*,
*Checks* and *Commit statuses* is enough. Either token is stored in the system keychain,
never in the configuration file, and only github.com is reached this way.

Bombadil never changes anything on GitHub.

## Turning it off

**Settings › GitHub pull requests › Don't watch** stops all lookups.
