# Git for repositories in provider environments

Source Control has one implementation. A repository that lives in an environment
provided by an extension (a remote machine, a container) uses the same model, tasks
and panels as a local one, with the same features. This document records how, so the
work can be done in stages that each leave the product consistent.

An earlier, reduced implementation for provided roots was removed because two
implementations drift apart. Do not reintroduce a second panel, model or command
set for environments.

## Principles

1. **One implementation.** Local and environment repositories share `features/git`,
   `app/git*` and `ui/git`. A feature is finished when it works for both.
2. **The application stays generic.** An environment is any provided root. The
   application MUST NOT branch on plugin ids, server programs or protocol names (see
   [Shared extension UI](shared-extension-ui.md)). What it needs from a provider is
   already in the workspace API: file operations and process execution.
3. **Git runs where the repository is.** Every `git` process for an environment
   repository is started in that environment, never on the client with a translated
   path.
4. **No silent fallback.** An environment without the process capability, without a
   trusted folder, without `git`, or disconnected, shows that state. It never falls
   back to a local repository of the same spelling.

## Repository identity

The model, snapshots, diff documents and UI key repositories by `PathBuf`. Changing
that to an enum touches most of the Git code, so identity stays a `PathBuf` and the
environment is encoded in it.

`features/git/location.rs` owns the encoding and nothing else interprets it:

- `encode(environment, provider path) -> PathBuf` and `decode(&Path) -> Option<(…)>`.
- The encoded path is **absolute on every platform** and cannot collide with a real
  path: `\\?\loog-remote\<environment>\<path…>` on Windows and
  `/.loog-remote/<environment>/<path…>` elsewhere. The path part is the provider's
  POSIX path split into components. Windows gets the verbatim form on purpose: a UNC
  form (`\\server\share`) would send every stray file call to the network to look for
  that server, where a verbatim path simply does not exist.
- A small registry, owned by the application, maps the `<environment>` token back
  to the environment key and scope. The token is not the key itself, so the key's
  spelling never has to be a legal path component.
- Round trip is exact for every provider path the workspace API accepts.
  Components that a client `OsString` cannot hold (non-UTF-8 names on Windows) are
  refused with a clear error, as the workspace API already requires UTF-8 names.

Joins, parents, `strip_prefix` and equality then work unchanged, so ordering, maps
and the UI code that handles repository paths keep working.

## Execution

`GitCommand.cwd` is already a `ResourceRef`. `GitCommandRunner` dispatches on it:

- `Local` runs the process as today.
- An encoded environment path is decoded and run through an injected executor.
  The application provides it from the provider's process capability, with the same
  environment variables and flags the local runner sets (no pager, no prompt, no
  color, no external diff, `--no-optional-locks` for reads).

The command surface, parsing and error mapping are shared. The runtime (`git`
executable) of an environment is the command name `git` resolved there.

## Filesystem access

The Git infrastructure reads a few files directly (`.git` markers, rebase state,
stash logs, working-tree files for untracked and conflict content). Each goes
through one small seam, `RepositoryFs`:

- Local: `std::fs`, as today.
- Environment: prefer a `git` command that returns the same fact (`rev-parse`,
  `stash list`, `status`), else a provider file operation with its size budgets.

Discovery uses `rev-parse` for environments and never walks directories by client
paths.

## Lifecycle

- **Discovery** takes workspace roots (local) and provided roots that have the
  process capability and are trusted. A repository inside a provided root is
  discovered through the environment.
- **Polling** uses the existing poller. An environment scan has its own interval,
  is skipped while the window is in the background, and stops for a disconnected
  environment.
- **Offline.** A repository whose environment disconnects stays in the panel as
  unavailable with a reason, keeps its commit message draft, and returns when the
  environment does.
- **Opening files and diffs.** Actions that open a file decode the repository path
  and open the provided resource. Diff documents keep their `PathBuf` identity.

## Where the code is

- `features/git/location.rs`: encode, decode, the environment token registry and
  `git_path`, which names a resource the way the Git model does.
- `infrastructure/git/remote.rs`: the `RemoteGit` seam the runner and file reads call
  for an environment path, and the helpers that make a remote command line match a
  local one (UTF-8 arguments, environment paths turned back into the environment's
  own, the same variables, never a prompt).
- `infrastructure/git/repo_fs.rs`: the one place the Git code reads a repository's
  files, local or environment. `Names` lists a directory once instead of asking about
  each name.
- `app/environment_git.rs`: backs `RemoteGit` with the connected root's process and file
  operations, and opens and copies environment files for Source Control.
- `app/git_poll.rs` `scan_roots`: local folders, the provided roots that may run
  processes, and the environment worktrees Source Control was told about.

## Behavior worth knowing

- A root that cannot answer (disconnected, not trusted, no `git` there) is skipped and
  never fails the scan of the other repositories. It is not shown as offline: the
  repository leaves Source Control until the environment is back.
- An environment has no trash. Discarding untracked files deletes them with `git clean`
  and the confirmation says so.
- Opening a worktree of an environment repository makes Source Control scan it; there
  is no workspace folder to add it to.
- Cloning into an environment works in the backend (a destination that is an
  environment path); the clone dialog still only offers local folders.

## Stages

Each stage ships on its own and keeps local behavior unchanged.

0. **Foundation.** Location encoding and registry; runner dispatch with an injected
   executor; the `RepositoryFs` seam with the local implementation; tests with a
   fake executor. No visible change.
1. **Read.** Discovery, status, the Changes panel, line decorations, opening diffs.
2. **Write.** Stage, unstage, discard, hunk and line apply, commit and amend,
   conflict resolution.
3. **History and refs.** Log and graph, commit details, branches, tags, stashes,
   worktrees, remotes.
4. **Network and the rest.** Fetch, pull, push, submodules, LFS, blame, sparse
   checkout, clone into an environment.

## Risks and answers

- **Latency.** One status refresh runs several `git` processes. Over a remote link
  that adds up. First measure, then batch commands into one provider call if needed.
- **Credentials.** There is no terminal, so prompts are off. Pushes that need a
  password fail with the error shown. Key-based remotes work. A credential flow is
  a separate feature.
- **Large repositories.** Existing size and time budgets apply, enforced on both
  sides.
- **Names that are not text.** Refused with a message rather than altered.
- **Windows host, Linux environment.** Separators, case sensitivity and drive
  letters live only in the location encoding; Git output is parsed as POSIX.
- **Trust.** Running `git` in a folder executes its hooks and config. Environment
  repositories follow the same trust decision that gates the process capability.

## Testing

Each stage has acceptance coverage that runs the real Git code against a fake
provider executor backed by a temporary local repository, so the shared code path
is exercised without a network. Local tests continue to run unchanged.
