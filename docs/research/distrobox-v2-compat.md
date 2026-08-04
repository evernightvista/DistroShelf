# DistroShelf vs distrobox v2.0.0-rc: migration & version-handling compat check

**Date:** 2026-08-04
**Upstream studied:** distrobox v2.0.0-rc, commit `2.0.0-rc.4-5-gd34399f`
(`git describe --tags`; HEAD `d34399f`, "chore(deps): bump golangci/golangci-lint-action from 9.2.1 to 9.3.0 (#2183)")
**Checkout location:** `/var/home/fedora/.var/app/com.ranfdev.DistroShelf/data/opencode/repos/github.com/89luca89/distrobox`
**v1 reference:** same checkout, tag `1.8.2.5` (shown via `git show 1.8.2.5:<file>`)
**Empirical verification:** the v2 binary was built from this checkout (`go build ./cmd/distrobox`) and exercised with a fake `podman` on PATH (see §B.4, §A.1). Runtime-level behavior WAS verified live against a real host podman 5.8.4 (rootless, crun, Fedora/SELinux) on 2026-08-04 — tests T1–T5 below, one new surprise finding (§A5: SELinux), and one new v2 defect (§A7: `DBX_SCRIPTS_DIR` + self-provisioning). Remaining caveats: only *rootless* podman/crun on an SELinux-enforcing host was exercised (docker and rootful podman were not re-tested), and the sandbox's `/tmp` is private (invisible to the host), so the live rig was relocated to a host-shared path — see Sources.

Verdict summary:

| Area | Verdict |
|------|---------|
| A. Migration (`distrobox_init_migration.rs`) | **Correct (live-verified).** v2 mounts the same three scripts at the same destinations, from the same directory, with the same un-dereferenced paths. Bind-mount sources resolve through symlinks at start time (both dir-level and per-script), and the stale path fails hard when missing — the migration's repair is both necessary and sufficient (T3, T4). One new caveat: SELinux — distrobox-created containers are unconfined (`spc_t`) because v2 passes `--privileged` + `--security-opt label=disable` (`podman.go:201-202`), which is also what lets them read `user_home_t` bind sources (T4). |
| B. Version handling (`root_store.rs`, `distrobox.rs`) | **Broken (live-confirmed).** `distrobox version` (subcommand) is gone in v2 — only `--version` remains, and its output has no colon. Both DistroShelf call sites fail against v2 (T1). |

---

## A. Migration correctness (`distrobox_init_migration.rs`)

**Verdict: correct (live-verified, T3–T5).** The whole stale-path concept carries over to v2 unchanged. Evidence per question:

### A1. Are the three scripts still bind-mounted?

Yes. v2's `Podman.makeCreateCommand` appends, in order:

```go
// pkg/containermanager/providers/podman.go:246-252
options = append(options, "--volume", fmt.Sprintf("%s:%s", distroboxExportPath, "/usr/bin/distrobox-export:ro"))
options = append(options, "--volume", fmt.Sprintf("%s:%s", distroboxHostexecPath, "/usr/bin/distrobox-host-exec:ro"))
// pkg/containermanager/providers/podman.go:452-453
options = append(options, "--volume", fmt.Sprintf("%s:%s", distroboxInitPath, "/usr/bin/entrypoint:ro"))
options = append(options, "--entrypoint", "/usr/bin/entrypoint")
```

The Docker provider does the same (`pkg/containermanager/providers/docker.go:241-245` and `docker.go:444-445`). The three source paths come from `filepath.Join(scriptsDir, "distrobox-init")` / `"distrobox-export"` / `"distrobox-host-exec"` (`podman.go:147-149`, `docker.go:142-144`).

Empirically confirmed with `distrobox create --dry-run --image alpine:latest --name testbox` on the built binary (fake podman):

```
--volume /tmp/opencode/dbx-build/distrobox-export:/usr/bin/distrobox-export:ro
--volume /tmp/opencode/dbx-build/distrobox-host-exec:/usr/bin/distrobox-host-exec:ro
--volume /tmp/opencode/dbx-build/distrobox-init:/usr/bin/entrypoint:ro
--entrypoint /usr/bin/entrypoint
```

These are byte-for-byte the same mount destinations as v1 (`git show 1.8.2.5:distrobox-create`, lines 731-733: `--volume "${distrobox_entrypoint_path}":/usr/bin/entrypoint:ro`, `...distrobox-export:ro`, `...distrobox-host-exec:ro`). DistroShelf's doc table (`src/distrobox_init_migration.rs:6-10`) and `ENTRYPOINT_MOUNT_DESTINATION` (`src/backends/container_runtime.rs:20`) are still accurate. Live-verified end to end against podman 5.8.4 (T3): `podman inspect` of a real distrobox-created container shows exactly these three `:ro` mounts plus the usual distrobox plumbing (home, `/run/host`, `/dev`, …).

### A2. Do the scripts still live next to the binary? Symlink or real file?

Yes to the location, and they are **real files**, not symlinks:

- Scripts dir resolution is `resolveScriptsDir()` in `pkg/config/config.go:272-290`: `DBX_SCRIPTS_DIR` env var → the running binary's directory (`os.Executable()` + `filepath.Dir`) → `~/.local/bin` → `/usr/bin`. The binary's dir is the default, exactly like v1's `hostDir()`.
- In v2 the scripts are **embedded** in the binary via `go:embed` and written to disk on demand (see A6). They are written with `os.WriteFile(..., 0755)` (`internal/inside-distrobox/scripts.go:63`) — plain files, not symlinks. The assets themselves are real files: `internal/inside-distrobox/assets/distrobox-init` (91 KB), `distrobox-export` (22 KB), `distrobox-host-exec` (6.8 KB), all `-rwxr-xr-x`.
- v1's `distrobox-init` was also a real file (`git ls-tree 1.8.2.5` shows blobs with mode `100755`), so the module doc's "symlink to the actual entrypoint script" question is answered "real file" in both versions. (Nothing about the migration depends on this — it only cares about paths.)

Naming note: the migration doc cites `hostDir()` in `internal/inside-distrobox/scripts.go` (`src/distrobox_init_migration.rs:13`, `docs/distrobox-init-migration.md:12`). In v2 the location logic moved to `resolveScriptsDir()` in `pkg/config/config.go`; `scripts.go` only contains `ProvisionScripts` + the embedded assets. The *behavior* is the same, the function name/package changed.

### A3. Is the in-container destination still `/usr/bin/entrypoint`?

Yes, hardcoded — `podman.go:452-453` and `docker.go:444-445` (quoted in A1). No configurability. DistroShelf's detection key (`Destination == "/usr/bin/entrypoint"`, `src/backends/container_runtime.rs:229-262`) remains correct.

### A4. Are the absolute host paths still baked into the container config at create time?

Yes. v2 passes the absolute source path verbatim as a `--volume` argument to `podman create`/`docker create` (A1). The runtime persists it in the container config, which is exactly what DistroShelf reads back via `podman inspect --format '{{ json .Mounts }}'` (`src/backends/container_runtime.rs:234-238`). Nothing re-resolves or rewrites these paths afterwards; `distrobox enter` just starts the existing container (`podman.go:573-587`). Upstream's own install script acknowledges the permanence of v1 paths explicitly: it installs the helpers "to … keep [v1-created] containers … finding a working bind-mount source" (`install:256-261`).

### A5. Does the runtime follow symlinks when resolving bind-mount sources?

Distrobox v2 never dereferences the script paths itself, so whatever path the migration symlinks at is exactly what the runtime receives:

- The only `filepath.EvalSymlinks` calls in the whole `pkg/` + `internal/` tree are for the *config file* path (`pkg/config/config.go:149`) and the `/dev/shm` special case (`podman.go:315`, `docker.go:322`). The init/export/host-exec paths are built with plain `filepath.Join(scriptsDir, name)` and passed straight into `--volume` (`podman.go:147-149` + `podman.go:246-252,452`).
- `os.Executable()` (used by `resolveScriptsDir`, `config.go:279-281`) resolves the *binary* via `/proc/self/exe` — i.e. if the `distrobox` binary itself is a symlink, its resolved target dir wins; this matches v1 and is already accounted for by DistroShelf's real-directory bundle strategy (`docs/distrobox-init-migration.md:363-370`).
- Distrobox's own `exists()` check uses `os.Stat` (symlink-following) to decide whether a script is already provisioned (`internal/inside-distrobox/scripts.go:73-82`) — i.e. upstream itself treats a symlinked script as a valid script.
- Whether podman/crun/docker then follow the symlink at *start* time is runtime behavior, not distrobox behavior. **Live-verified against host podman 5.8.4 (rootless, crun) on 2026-08-04 (T3, T4).** Results:
  - **Missing source = hard failure, no auto-create (T4.3).** With the baked source paths pointing at a non-existent directory, `podman start` fails and creates nothing:
    ```
    $ podman start dbx-v2-compat-test
    Error: unable to start container "bbeca442a524…": crun: cannot stat
    /…/dbx-verify/bin/distrobox-init: No such file or directory: OCI runtime
    attempted to invoke a command that was not found
    ```
    (exit code 125; the directory at the stale path is NOT recreated — verified `ls` shows no auto-created dir). This is the exact broken state `find_stale_containers` detects and `migrate_stale_path` repairs.
  - **Symlinked sources ARE followed at start time (T4.6, T4.9).** Both the dir-level shape (`bin -> bin.real`, mount source `…/bin/distrobox-init`) and the per-script shape (three individual symlinks, exactly what `migrate_stale_path` creates at `distrobox_init_migration.rs:340-371`) started the container and ran the full init: logs reach `container_setup_done`. Evidence the kernel resolved the link: `mountinfo` inside the running container shows the source path fully dereferenced (`/home/fedora/.var/app/…/bin.real/distrobox-init` for a source specified through the symlink).
  - **SELinux surprise (T4.7-4.9): the first per-script attempt failed with `Permission denied` — but not because of symlinks.** Plain `podman create` containers run in the `container_t` domain and get EACCES on any bind source labelled `user_home_t` (this host's context for `~/.var/…`), while the distrobox-created container runs `unconfined_u:unconfined_r:spc_t` and reads the same files fine. Cause: v2 passes `--privileged`, `--security-opt label=disable`, `--security-opt apparmor=unconfined` on every create (`podman.go:201-202`) — i.e. *distrobox containers are SELinux-unconfined by design*, which is what makes the migration's symlinked binds readable on SELinux hosts. Replicating the per-script symlink mount with `--security-opt label=disable` (mirroring distrobox's own flags) started the container and reached `container_setup_done`. Caveat: a v2 container that a user later re-creates *without* those flags (e.g. plain `podman create`) will hit EACCES on `user_home_t` sources regardless of symlinks — a pre-existing SELinux property of bind mounts, not a migration regression.
  - Net: the migration's symlink repair is **safe and sufficient** on this stack; nothing about the mechanism changed vs the v1 rationale (`docs/distrobox-init-migration.md:166-178`).

### A6. How are the scripts provisioned in v2?

Both mechanisms now exist:

1. **Install time** (unchanged shape): the `install` script installs the three helpers as real files next to the binary (`install:262-264`). The comment at `install:256-261` explicitly notes the Go binary embeds them and would extract them at first use, but shipping them avoids extraction and keeps v1 paths valid.
2. **On demand at create time (new in v2):** the three scripts are embedded (`//go:embed assets/…`, `internal/inside-distrobox/scripts.go:29-36`) and `ProvisionScripts()` (`scripts.go:40-69`) writes any missing one into `ScriptsDir` before `makeCreateCommand` runs (`podman.go:112`, `docker.go:109`). `exists()` (`scripts.go:73-82`) skips writing when the script already sits next to the binary.

Empirically verified: after one `create --dry-run` with the freshly built binary (which had no scripts next to it), `distrobox-init`, `distrobox-export` and `distrobox-host-exec` appeared in the binary's directory with mode 0755. Re-verified live on a real `distrobox create` against podman 5.8.4 (T3): the three scripts were written next to the binary (0755) *before* `podman create` ran, and the mount Sources in `podman inspect` point into that directory.

Consequence for the migration: the stale-path scenario can still happen (the old directory is gone and v2 only provisions into the *current* scripts dir), but a v2-only install is slightly more self-healing than v1. The migration's sibling repair (`distrobox-*` glob) is unaffected: the same three scripts are provisioned.

### A7. Anything changed that would break the migration?

Nothing found. Specifically:

- **No new scripts are mounted** — the mount set is exactly the three (`podman.go:246-252,452`, `docker.go:241-245,444`). The `distrobox-` prefix glob used by the migration (`src/distrobox_init_migration.rs:173-179`) still covers exactly the mounted set.
- **`distrobox-init` was not replaced by a Go binary** — it is still the POSIX shell entrypoint (91 KB script, `internal/inside-distrobox/assets/distrobox-init`); the architecture doc confirms ("Such commands are POSIX shell scripts that are included as assets", `docs/posts/distrobox_next_architecture.md:135-144`).
- **Entrypoint args are compatible.** v2 bakes `--verbose --name --user --group --home --init --nvidia --pre-init-hooks --additional-packages -- <init-hook>` (`podman.go:462-473`), and the v2 init script parses exactly those flags (`distrobox-init:120-195`, incl. `--additional-packages` and `--`). v1's init accepted the same names (`git show 1.8.2.5:distrobox-init` lines 96-108, 132-178). So a container created by v1 and migrated to a v2 init keeps working arguments-wise (the caveat in `docs/distrobox-init-migration.md:331-337` remains theoretical).
- **Mount options unchanged** (`:ro`; `BindPropagation()` = `:rslave` only for the other bind mounts, `pkg/containermanager/containermanager.go:44-61`).
- **`distrobox upgrade` depends on the same mounted entrypoint**: v2's upgrade runs `/usr/bin/entrypoint --upgrade` *inside* the container (`pkg/commands/upgrade.go:33`), so a stale entrypoint mount breaks upgrade too — which is consistent with the migration's "whole directory vanished" rationale (`src/distrobox_init_migration.rs:14-20`).
- **New wrinkle — confirmed, and worse than "non-breaking" (T5):** v2's `DBX_SCRIPTS_DIR` env var can point the scripts dir *away* from the binary's directory (`config.go:272-276`). v1 had no such override (no `DBX_SCRIPTS_DIR` in any `1.8.2.5` file). Live-verified against podman 5.8.4:
  - With a *fresh* binary (no scripts next to it) and `DBX_SCRIPTS_DIR` set, the scripts are provisioned into the override dir and the container's mount Sources bake the override path (`…/dbx-scripts/distrobox-init` etc.) — so DistroShelf's `current_init_path()` "sibling of the binary" assumption (`src/distrobox_init_migration.rs:82-88`) indeed computes the wrong path on such hosts. The migration would then treat the baked path as "current" and never classify the container stale.
  - New defect found: when the three scripts **already exist next to the binary** (i.e. after any normal first use), `DBX_SCRIPTS_DIR` breaks `create` outright. `ProvisionScripts` writes into `ScriptsDir` but `exists()` short-circuits on the *binary's* dir (`scripts.go:57-59` + `73-82`), so nothing is written to the override dir and `podman create` fails with `Error: statfs …/dbx-scripts/distrobox-init: no such file or directory` (exit 1). Empirically reproduced (T5). Edge case — only affects users who set the env var, but for them v2 create is either wrong-path-baked (fresh binary) or completely broken (after first use).

---

## B. Version handling correctness

**Verdict: broken.** `distrobox version` no longer exists as a subcommand, and the replacement (`--version`) prints a colon-less string that `split(':').nth(1)` cannot parse. Both DistroShelf version probes fail against v2, and the app would treat v2 as "no distrobox available" (falling to the Welcome view, per `src/models/root_store.rs:279-310`).

### B1. What does `distrobox version` print in v2?

The **subcommand is gone**. `subcommands()` in `internal/cli/root.go:190-200` registers exactly: `assemble, create, enter, ephemeral, generate-entry, list, rm, stop, upgrade` — no `version`. The only version interface is the root `--version`/`-V` flag (`root.go:79-85`, `Version: version.Version` at `root.go:90`), whose value is injected at build time via ldflags (`Makefile:3-4`: `-X github.com/89luca89/distrobox/pkg/version.Version=$(VERSION)`; default `"dev"`, `pkg/version/version.go:22-23`).

Empirically verified on the built binary (fake podman; re-run live with real podman 5.8.4 on PATH, T1 — identical output, exit codes 0 and 3):

```
$ distrobox --version
distrobox version dev            (release builds: "distrobox version 2.0.0-rc.4" — verified with -X ldflags)
$ distrobox version
No help topic for 'version'       (stderr; exit code 3)
```

So even if DistroShelf switched to `--version`, the output `distrobox version 2.0.0-rc.4` contains **no colon**, and `split(':').nth(1)` (`src/models/root_store.rs:333`, `src/backends/distrobox/distrobox.rs:761-762`) would still fail. The v2 parsing would need a `split(' ')` + last-token (or similar) approach. For context, v1 printed `distrobox: 1.8.2.5` for `distrobox version`/`--version`/`-V` (`git show 1.8.2.5:distrobox`, case `-V | --version | version)` → `printf "distrobox: %s\n" "${version}"`), which is what the `split(':').nth(1)` parser was written against.

Bonus detail: the *in-container* script `distrobox-init` still prints `distrobox: <version>` for `-V/--version` (`internal/inside-distrobox/assets/distrobox-init:133` — same in v1, `git show 1.8.2.5:distrobox-init:129`), but DistroShelf probes the host binary, not the script, so this doesn't help.

### B2. Does v2 accept `distrobox version` as a command?

No (see B1). Exit 3, "No help topic for 'version'". The v1-style `distrobox-version` symlink is also absent — the install script only creates symlinks for `assemble create enter ephemeral generate-entry ls list rm stop upgrade` (`install:266-271`), and `ResolveArgs()` (`root.go:54-67`) would map `distrobox-version` to `distrobox version`, which fails the same way.

### B3. Flag-by-flag comparison (v2 vs DistroShelf usage)

Sources: `internal/cli/create.go:71-193`, `enter.go:41-88`, `rm.go:41-63`, `stop.go:48-60`, `upgrade.go:48-63`, `list.go:41-46`, `root.go:278-306`.

| DistroShelf invocation | v2 status | Notes |
|---|---|---|
| `distrobox version` (`root_store.rs:324-325`, `distrobox.rs:759`) | **REMOVED** | No `version` subcommand (`root.go:190-200`); only `--version/-V` flag. |
| `ls --no-color` (`distrobox.rs:701`) | present | `--no-color` BoolFlag, `list.go:42-46`; name `list`, alias `ls` (`list.go:37-40`). Auto-disabled when stdout isn't a TTY (`list.go:65`). |
| `create --yes` (`domain.rs:325`) | present | `--yes/-Y`, `create.go:95-100`. |
| `create --image` | present | `--image/-i`, `create.go:72-77`. |
| `create --name` | present | `--name/-n`, `create.go:78-83`. |
| `create --hostname` | present | `create.go:84-88`. |
| `create --init` + `--additional-packages systemd` (`domain.rs:335-338`) | present | `--init/-I` `create.go:136-142`; `--additional-packages/-ap` `create.go:123-127`. |
| `create --root` (`domain.rs:340-342`) | present | `--root/-r` added by `withRoot`, `root.go:278-306`. |
| `create --no-entry` (`domain.rs:343-345`) | present | `create.go:175-178`. |
| `create --nvidia` | present | `create.go:143-146`. |
| `create --home` | present | `--home/-H`, `create.go:108-113`. |
| `create --volume` | present | `create.go:114-117`. |
| `create --clone` (`distrobox.rs:688-697`) | present | `--clone/-c`, `create.go:101-107`. |
| `create --pull` (`domain.rs` args) | present | `--pull/-p`, `create.go:89-94` ("implies --yes"). |
| `create --compatibility` (`distrobox.rs:669`) | present, **semantics changed** | See §C.3. |
| `enter --name <c> -- <cmd>` / `enter <c> -- <cmd>` (`distrobox.rs:541,556,608`, etc.) | present | `--name/-n` `enter.go:42-47`; `--` handled by `PrepareArgs`/`splitExecCommand` (`parse.go:34-135`); `-e/--exec` also exists (`enter.go:48-54`). Verified with `--dry-run`. |
| `rm --force <name>` (`distrobox.rs:728`) | present | `--force/-f`, `rm.go:47-51`. |
| `stop --yes <name>` / `stop --all --yes` (`distrobox.rs:734,739`) | present | `--yes/-Y`, `--all/-a`, `stop.go:48-60`. |
| `upgrade <name>` / `upgrade --all` (`distrobox.rs:745,751`) | present | `--all/-a`, `--running`, `--yes/-Y` "accepted for compatibility" (`upgrade.go:48-63`). |

No other DistroShelf-used flag changed name or meaning.

### B4. Does `distrobox ls` output change in v2?

No — same shape. `printResult` uses `"%-12s | %-20s | %-18s | %-30s\n"` with header `ID | NAME | STATUS | IMAGE` (`internal/cli/list.go:71-91`). Empirically captured (fake podman returning `ps -a --no-trunc --format json`):

```
ID           | NAME                 | STATUS             | IMAGE
a1b2c3d4e5f6 | archlinux            | Exited (0) 5 minutes ago | docker.io/library/archlinux:latest
d24405b14180 | ubuntu with space    | Up 2 minutes ago   | ghcr.io/ublue-os/ubuntu-toolbox:latest
```

Live-verified against real podman 5.8.4 (T2): a real running container (`alpine-vecchissimo`, `Up 14 hours`) renders in the identical table shape:

```
ID           | NAME                 | STATUS             | IMAGE
cb7ff42b880b | alpine-vecchissimo   | Up 14 hours        | docker.io/library/alpine:latest
```

`--no-color` holds with a bogus `TERM` and through a pipe (no ANSI escapes in the output; T2).

Notes for DistroShelf's parser (`split('|')` + trim, exactly 4 fields, `src/backends/distrobox/domain.rs:158-197`):

- Names with spaces are **not quoted**, but the padded columns keep `|` as a reliable separator, so the parser still works. (A literal `|` inside a name would break it — same as v1.)
- ID is truncated to 12 chars (`podman.go:677-684`); STATUS is the raw podman status (`Up …`, `Exited (…)`, `Created …`) — matches `Status::from_str`'s `Up`/`Exited`/`Created` prefixes (`domain.rs:128-138`).
- v2 lists only distrobox-owned containers (`manager=distrobox` label or any `distrobox.*` label key, `pkg/containermanager/containermanager.go:130-150`) and **sorts by name** (`pkg/commands/list.go:61-64`). The header is always printed, even with zero containers — DistroShelf's `lines().skip(1)` (`distrobox.rs:703`) is safe.

---

## C. Other breaking changes relevant to DistroShelf

### C1. Runtime abstraction & events

v2 auto-detects **podman → podman-launcher → docker** only (`pkg/containermanager/providers/autodetect.go:33-44`); lilipod appears only in help text (`root.go:282`). DistroShelf's own runtime layer probes podman/docker directly and streams `podman events --format json` (`src/backends/container_runtime.rs:336-358`, `src/backends/podman.rs:67-80`) — that traffic is between DistroShelf and the runtime, never touches distrobox, so the Go rewrite does not affect it. Events, `ls` refresh triggers, `inspect`, `stats`, `images --format json` all remain runtime-level and unchanged.

### C2. Upgrade/migration docs in the v2 checkout

`docs/posts/announcing_distrobox_next.md:50-57` is the relevant compatibility statement:

> "v2 maintains the same interface for CLI command arguments, manifest files, and configuration files. … Existing v1 containers work with v2, **except for exported bins and apps — those containers must be recreated**. v2 ships as a single binary, so command-specific executables like `distrobox-enter` and `distrobox-create` no longer exist."

Note: the "no separate executables" claim is softened in practice by the install script's v1-compat symlinks (`install:266-271`) and `ResolveArgs()` basename dispatch (`root.go:44-67`); DistroShelf always invokes the `distrobox` binary with subcommands, so it is unaffected either way. The "exported bins/apps must be recreated" claim concerns v1 containers with exports; the migration's symlink repair does not address it (and was never intended to).

No formal 1.x→2.x changelog exists in the checkout's `docs/` (only `compatibility.md`, `useful_tips.md`, usage pages, and the two `next` posts).

### C3. `create --compatibility` behavior change

v1: `--compatibility` printed a hardcoded image list immediately, no container manager needed (`git show 1.8.2.5:distrobox-create:350-353`).

v2: the flag still exists (`create.go:188-192`), but the list is **fetched over HTTP** from `docs/compatibility.md` on GitHub (ref derived from the build version, 15 s timeout) and cached at `$XDG_CACHE_HOME/distrobox/distrobox-compatibility-<ref>` (`internal/cli/compatibility.go:40-56, 88-111, 196-206`). Additionally the `withContainerManager` pre-hook runs **before** the compatibility branch (`root.go:312-343`), so without a detected container manager the command fails with "Missing dependency" — empirically verified. DistroShelf's `list_images()` (`distrobox.rs:667-682`) parses one-image-per-line, which the v2 output still satisfies (verified), but first-run behavior now requires (a) a container manager installed and (b) network access.

### C4. Configuration files (not used by DistroShelf, listed for completeness)

v2 reads `distrobox.conf` files plus `~/.distroboxrc`, parsed as **INI, not shell-sourced** (`pkg/config/config.go:169-178` and the explicit note at `config.go:205-210`). v1 had no config-file support at all in 1.8.2.5 (no matches for `.conf`/`.distroboxrc` in `git show 1.8.2.5:distrobox`). Environment-variable configuration (`DBX_*`) carries over (`config.go:215-268`).

### C5. stop/rm/upgrade semantics

- `stop`: `podman stop` on each name; `--all` uses the distrobox list (`pkg/commands/stop.go`). Same behavior as v1.
- `rm`: `podman rm --volumes [--force]` (`podman.go:626-651`) — the `--volumes` flag matches v1's removal of the anonymous `/var/log/journal`, `/dev/pts` volumes.
- `upgrade`: runs `su-exec root /usr/bin/entrypoint --upgrade || doas … || sudo -S …` inside the container (`pkg/commands/upgrade.go:33`), i.e. it invokes the **mounted** entrypoint — same dependency on the bind-mount paths as v1, and the same reason the stale-init migration must keep working under v2.

### C6. Version-string caveats for the bundled-distrobox path

- The release version string of a v2 build is a `git describe` output such as `2.0.0-rc.4` (or `2.0.0-rc.4-5-gd34399f` for this commit) — `Makefile:3-4`. DistroShelf's `parse_semver` (`src/distrobox_downloader.rs:18-27`) cannot parse `2.0.0-rc.4` (segment `0-rc` is not `u32`), so `is_bundled_update_available()` would report "no update" and legacy-dir discovery (`find_latest_legacy_version_dir`, `distrobox_downloader.rs:130-157`) would silently skip a `distrobox-2.0.0-rc.4/` directory.
- DistroShelf's bundle provisioning (`distrobox_downloader.rs:164-291`) downloads the *source* tarball of `1.8.2.5` (which contains the `distrobox` shell script + the three helpers at the tarball root — exactly the layout `resolve_bundled_distrobox_path` and the migration expect). A v2 source tarball does **not** contain a root `distrobox` executable (the Go source lives at `cmd/distrobox/`), and the v2 release artefact `distrobox-linux-<arch>.tar.gz` contains only the binary (the install script takes the helpers from the source tarball, `install:250-264`). Bundling v2 would require a new provisioning path — but note v2 would self-provision the three scripts next to the binary on first `create` anyway (`scripts.go:40-69`).

---

## Sources

All citations are file:line into the v2 checkout at `2.0.0-rc.4-5-gd34399f` (or DistroShelf), or `git show 1.8.2.5:<file>` for v1. Everything in this document was read from these files or produced by running the built binary; no secondary sources were used.

### Upstream v2 (checkout root = `/var/home/fedora/.var/app/com.ranfdev.DistroShelf/data/opencode/repos/github.com/89luca89/distrobox`)

- `pkg/containermanager/providers/podman.go:112, 147-149, 246-252, 315, 452-453, 462-473, 626-651, 677-684` — script provisioning call, script path join, the three bind mounts, `/dev/shm` symlink special case, entrypoint mount + `--entrypoint`, baked init args, `rm`/`stop`, ID truncation.
- `pkg/containermanager/providers/docker.go:109, 142-144, 241-245, 322, 444-445` — same mounts for Docker.
- `internal/inside-distrobox/scripts.go:29-36, 40-69, 73-82` — embedded assets, `ProvisionScripts`, `exists()`.
- `internal/inside-distrobox/assets/distrobox-init:120-195, 133` — init arg parsing; `-V` prints `distrobox: <version>`.
- `pkg/config/config.go:55-59, 125, 149, 169-178, 205-210, 215-268, 272-290` — `ScriptsDir`, `resolveScriptsDir`, config file paths + INI parsing, `DBX_*` env vars.
- `internal/cli/root.go:54-67, 79-90, 190-200, 278-306, 312-343` — `ResolveArgs`, `--version` flag, subcommand list (no `version`), `--root`, container-manager pre-hook.
- `internal/cli/create.go:71-193, 200-207` — all create flags; compatibility branch.
- `internal/cli/compatibility.go:40-56, 88-111, 196-206` — network fetch + cache of `--compatibility`.
- `internal/cli/list.go:37-51, 65, 71-91` — `ls`/`--no-color` + table format.
- `pkg/commands/list.go:48-67` — distrobox-only filter, name sort.
- `internal/cli/enter.go:41-95`, `rm.go:41-63`, `stop.go:48-60`, `upgrade.go:48-63` — enter/rm/stop/upgrade flags.
- `internal/cli/parse.go:34-135` — `--` splicing for `enter`.
- `pkg/commands/upgrade.go:33` — `/usr/bin/entrypoint --upgrade` script.
- `pkg/containermanager/containermanager.go:44-61, 106-110, 130-155` — bind propagation, `ScriptsDir`, `IsDistrobox`/`IsRunning`.
- `pkg/containermanager/providers/autodetect.go:29-44` — podman > podman-launcher > docker.
- `pkg/version/version.go:22-23` — `Version` var; `Makefile:3-4` — ldflags injection.
- `install:256-271` — v2 install: helpers installed next to binary; v1-compat subcommand symlinks.
- `docs/posts/announcing_distrobox_next.md:50-57`; `docs/posts/distrobox_next_architecture.md:135-144` — compatibility statement; embedded POSIX scripts.
- Empirical runs of `go build ./cmd/distrobox` (Go 1.26.5): `--version` output, `version` subcommand failure (exit 3), `create --dry-run` full podman command, `ls --no-color` against a fake `podman`, on-demand script provisioning.

### Live verification against real podman (2026-08-04)

All live tests ran the built v2 binary against host podman 5.8.4 (rootless, crun) on a Fedora/SELinux host, reached via `flatpak-spawn --host podman`, with a `podman` shim on PATH. **Environmental note:** the Flatpak sandbox's `/tmp` is a private tmpfs (invisible to the host — verified by comparing `ls /tmp/opencode` inside vs outside the sandbox), so the original rig at `/tmp/opencode/dbx-v2-bin/` cannot be bind-mounted by host podman; the rig was relocated to the host-shared path `/var/home/fedora/.var/app/com.ranfdev.DistroShelf/data/opencode/dbx-verify/` (same inodes on both sides) and all T3–T5 results were obtained there. The original binary + shim were left at `/tmp/opencode/dbx-v2-bin/` and `/tmp/opencode/podman-shim/` (sandbox-side, for follow-up).

- **T1** — `--version` prints `distrobox version dev` (exit 0); `version` prints `No help topic for 'version'` to stderr (exit 3). Confirms §B1 live.
- **T2** — `ls --no-color` against real podman: header `ID | NAME | STATUS | IMAGE`, real running container listed; output clean through a pipe and with a bogus `TERM`. Confirms §B4.
- **T3** — real `distrobox create --image alpine:latest --name dbx-v2-compat-test --yes`: scripts self-provisioned next to the binary (0755) before create; `podman inspect` shows exactly three bind mounts (`/usr/bin/entrypoint`, `/usr/bin/distrobox-export`, `/usr/bin/distrobox-host-exec`, all `:ro`, `Options:["rbind"]`) with Sources verbatim from the binary dir; `.Config.Entrypoint = [/usr/bin/entrypoint]`; container left in `created` state. Confirms §A1-A4, §A6.
- **T4** — stale-path simulation on the T3 container: (1) start with intact sources → Up, init runs to `container_setup_done`; (2) rename scripts dir → start fails: `crun: cannot stat …: No such file or directory: OCI runtime attempted to invoke a command that was not found`, exit 125, **no auto-created directory**; (3) symlink the stale path to the current dir → start succeeds, init runs to `container_setup_done`; (4) per-script symlinks (exact `migrate_stale_path` shape) with distrobox's own flags → start succeeds, init runs to `container_setup_done`; (5) mountinfo shows the kernel dereferences the symlink (source listed as `…/bin.real/distrobox-init` for both shapes). **SELinux finding:** plain `podman create` containers (`container_t`) get EACCES on `user_home_t` bind sources; distrobox containers run `unconfined_u:unconfined_r:spc_t` because v2 passes `--privileged --security-opt label=disable --security-opt apparmor=unconfined` (`podman.go:201-202`); with `--security-opt label=disable` the per-script symlink mounts work identically. Confirms §A5 and documents the SELinux interaction.
- **T5** — `DBX_SCRIPTS_DIR=<dir> distrobox create` with a fresh binary: scripts provisioned into the override dir and mount Sources bake the override path (wrong-path case for DistroShelf's sibling-of-binary `current_init_path()`); with scripts already next to the binary, create fails with `Error: statfs <override>/distrobox-init: no such file or directory` (exit 1) because `exists()` checks the binary dir while `ProvisionScripts` writes to the override dir. Confirms and extends §A7.

Host restored after testing: all test containers removed (`podman rm --force`), rig symlinks/dirs cleaned up, only the pre-existing `alpine-vecchissimo` remains.

### v1 reference (same repo, `git show 1.8.2.5:`)

- `distrobox` — `-V | --version | version)` case printing `distrobox: <version>`.
- `distrobox-create:95-104` (script paths from own dir), `:350-353` (compatibility without container manager), `:731-733` (the three `--volume` mounts).
- `distrobox-init:96-108, 129, 132-178` — init flags and `distrobox: %s` version output.
- `git ls-tree 1.8.2.5` — `distrobox-init`/`-export`/`-host-exec` are real files (mode 100755).

### DistroShelf (workspace root = `/var/home/fedora/Projects/DistroShelf`)

- `src/distrobox_init_migration.rs:6-10, 13, 82-88, 171-179, 189-236, 297-383` — migration logic + assumptions.
- `docs/distrobox-init-migration.md:5-15, 22-40, 105-110, 166-178, 308-327, 331-337, 363-370` — design rationale.
- `src/models/root_store.rs:321-351` — host version probe (`distrobox version`, `split(':').nth(1)` at :333); `:279-310` — Welcome fallback on version failure.
- `src/backends/distrobox/distrobox.rs:699-724` (`ls --no-color`), `:757-777` (`version()`), `:658-697` (create/clone), `:726-753` (rm/stop/upgrade), `:667-682` (`list_images`).
- `src/backends/distrobox/domain.rs:128-138, 158-197, 324-349` — status + ls parsing, create command.
- `src/backends/container_runtime.rs:20, 229-262, 336-358` — entrypoint mount destination/source, runtime detection.
- `src/backends/podman.rs:67-80` — `podman events --format json`.
- `src/distrobox_downloader.rs:13, 18-27, 118-157, 164-291` — bundle version `1.8.2.5`, semver parsing, legacy-dir resolution, download/install.
