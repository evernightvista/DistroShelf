# Migrating containers to a new `distrobox-init`

## Background

When a Distrobox container is created, the host's `distrobox-init` script is **bind‑mounted** into the container as the entrypoint:

```
--volume /host/path/to/distrobox-init:/usr/bin/entrypoint:ro
--entrypoint /usr/bin/entrypoint
```

The host path is determined at creation time by `hostDir()` in
`internal/inside-distrobox/scripts.go` and is **baked into the container's
persistent config**.  It is never re‑resolved or re‑applied — `distrobox enter`
simply runs `podman start` on the existing container.

Because the path is fixed, any change to the location (or removal) of the host's
`distrobox-init` file will break the container.

### Three sibling scripts are bind-mounted, not just `distrobox-init`

Distrobox actually provisions **three** sibling scripts in `hostDir()` (see
`ProvisionScripts()` in `internal/inside-distrobox/scripts.go`) and bind-mounts
all of them read-only into every container (see `makeCreateCommand()` in
`pkg/containermanager/providers/podman.go` and `docker.go`):

| Host script           | In-container destination          |
|-----------------------|-----------------------------------|
| `distrobox-init`      | `/usr/bin/entrypoint:ro`          |
| `distrobox-export`    | `/usr/bin/distrobox-export:ro`    |
| `distrobox-host-exec` | `/usr/bin/distrobox-host-exec:ro` |

All three sources are baked into the container config at creation time, and all
three live in the same directory. When that directory disappears, **all three
sources vanish together**. Repairing only `distrobox-init` lets the entrypoint
resolve, but the container still cannot invoke `distrobox-export` (used for
app/binary export) or `distrobox-host-exec` (used to run host commands from
inside the container) — those bind-mount sources are still missing, so Podman
either refuses to start or creates empty directories at the destinations. The
migration must therefore repair every `distrobox-*` sibling in the same pass.

## Two scenarios where the path becomes stale

### 1. Upgrading the bundled distrobox (pre‑stable‑path releases)

Older versions of DistroShelf extracted the bundled distrobox into a
**version‑specific directory** such as:

```
~/.local/share/distroshelf/distrobox-1.7.2/distrobox-init
```

Later DistroShelf versions adopted a **stable path** (`distrobox-bundled/`,
no version in the name).  When DistroShelf updates the bundle, it replaces the
`distrobox-bundled/` directory in place.  Containers created with the current
stable path continue to work because the bind‑mount source does not move.

However, containers created by the old version‑specific bundle still reference
the now‑deleted `distrobox-1.7.2/` directory.  Their entrypoint bind‑mount
points to a non‑existent file, and the container will not start.

### 2. Switching the distrobox source (bundled ↔ host)

DistroShelf lets the user choose between the **bundled** distrobox and the
**host** distrobox.  Containers created with one source carry a bind‑mount that
points to that source's `distrobox-init` path:

| Source   | Typical path baked into container |
|----------|-----------------------------------|
| Bundled  | `~/.local/share/distroshelf/distrobox-bundled/distrobox-init` |
| Host     | `/usr/bin/distrobox-init` (or `~/.local/bin/…`) |

Switching the setting in DistroShelf does **not** retroactively update existing
containers — they keep pointing to the original location.  If the original
location disappears (e.g. the bundled directory is cleaned up, or the system
distrobox is uninstalled), the containers break.

| Migration direction       | Works automatically? | Why |
|---------------------------|----------------------|-----|
| Host-old → Host-new       | Yes | System package manager updates in-place at `/usr/bin/` |
| Bundled-old → Bundled-new | Yes (with stable path) | `distrobox-bundled/` is replaced in-place |
| Bundled-old-versioned → Bundled-new | **No** | Old versioned directory (`distrobox-1.7.2/`) is gone |
| Host → Bundled            | **No** | Hard — `/usr/bin/` is root‑owned, can't symlink |
| Bundled → Host            | Yes (with symlink fix) | User‑owned paths accept symlinks, and host path is valid |

### Upstream has no migration mechanism

Upstream distrobox provides no tools to update a container's bind‑mount paths
after creation.  There is no `distrobox repair` command, no version label on
containers, and no version‑comparison check at start time.  The only escape
hatch is the `DBX_SCRIPTS_DIR` environment variable, which can override where
`hostDir()` provisions scripts — but this is a creation‑time override, not a
retrospective fix for existing containers.

### How NixOS solves the same problem

NixOS installs each distrobox version to an immutable, hash‑addressed store
path (`/nix/store/<hash>-distrobox-1.8.2.5/`).  Without intervention, `hostDir()`
would return that store path, and containers would break on every upgrade when
the old store path gets garbage collected.

NixOS avoids this by exposing distrobox through a **stable symlink**: the binary
at `/run/current-system/sw/bin/distrobox` is a symlink that the OS atomically
repoints to the current version's store path on every system activation.  Because
`os.Executable()` resolves `/proc/self/exe`, the binary wrapper must preserve
`$0` pointing to the **symlink path** (not the underlying real path) so that
`hostDir()` returns the stable location:

```
--volume "/run/current-system/sw/bin/distrobox-init":/usr/bin/entrypoint:ro
```

This is the same strategy as DistroShelf's `distrobox-bundled/` real directory:
a path that does not change across upgrades.  NixOS relies on the OS‑level
symlink; DistroShelf uses a fixed‑name directory.  Both work because the path
baked into the container outlives the current distrobox version.

## Why not recreate or remount the container?

### Container runtimes cannot modify an existing container's mounts

Neither `podman` nor `docker` offer a way to change a container's volume
mounts or entrypoint after creation.  The only option is to **delete and
recreate** the container.

### Recreating the container from scratch is fragile

The container's writable layer (its filesystem) is separate from the
distrobox‑managed mounts.  To preserve the user's data, we would need to:

1. Stop the container.
2. Commit its filesystem to a temporary image.
3. Extract the original creation parameters (name, image, volumes, init
   flags, …) from `podman inspect`.
4. Delete the container.
5. Create a new container from the temporary image with fresh bind‑mounts.
6. Remove the temporary image.

This is error‑prone: any mismatch in the reconstructed parameters could
produce a subtly different container.  It is also heavyweight, requiring a
full filesystem commit.

### `distrobox assemble --replace` deletes the filesystem

`assemble --replace` simply calls `rm` then `create` — the same
delete‑and‑recreate problem, but without the intermediate commit step, so
the container's filesystem is lost.

## Why symlinks work

The path to `distrobox-init` in a container is just a filesystem path that
Podman resolves at **start time** when mounting the volume.  If the path
exists and is readable, the container will start — Podman follows regular
filesystem symlinks during bind‑mount resolution.

Critically, the distrobox Go binary does **not** apply `realpath` or
`filepath.EvalSymlinks` to the init‑script path before passing it to
`--volume`.  The only symlink resolution in the path chain happens during
`os.Executable()` (which resolves the distrobox **binary** itself via
`/proc/self/exe`).  The script path is simply `filepath.Join(dir, name)`
with no further canonicalisation.

This means we can repair a stale container by placing a **symlink** at the
old path that points to the current `distrobox-init`:

```
# Old container expects init at:
~/.local/share/distroshelf/distrobox-1.7.2/distrobox-init

# We create a symlink:
~/.local/share/distroshelf/distrobox-1.7.2/distrobox-init
  → ~/.local/share/distroshelf/distrobox-bundled/distrobox-init
```

When Podman starts the container, it follows the symlink and mounts the new
init script.  The container's data is untouched, and no recreation is
necessary.

The same symlink strategy applies to the two sibling scripts
(`distrobox-export`, `distrobox-host-exec`): since they live alongside
`distrobox-init` in `hostDir()`, repairing the entrypoint's directory must
also repair its siblings, or the container will start but fail as soon as it
tries to invoke them.

## The porting strategy

When DistroShelf detects that the active `distrobox-init` location has changed
(either because the bundle was upgraded or the user switched sources), it will:

1. **Inspect every existing container** via `podman inspect` to find the
   host‑side bind‑mount whose destination is `/usr/bin/entrypoint`.
   See [Inspect output format](#podman-inspect-output-format) below.

2. **Skip running containers.**  Inspecting a running container is safe (the
   mount metadata is static), but creating a symlink while the container is
   starting introduces a narrow race where the stale path may already have
   been resolved and failed.  Stop the container first or defer the fix
   until it exits naturally.

3. **Compare** each container's bind‑mount source path against the current
   `distrobox-init` location (as returned by `hostDir()`).

4. **If the paths differ**, create the parent directory tree (`mkdir -p`) and
   place a **symlink** at the container's expected path pointing to the
   current `distrobox-init`. Then enumerate the current bundle directory
   (`ls -1`) and place a matching symlink for every other `distrobox-*` file
   present (e.g. `distrobox-export`, `distrobox-host-exec`, plus any script
   added by future upstream releases). `ln -sfn` (force, no-dereference) is
   used so re-running on an already-migrated container is a no-op:

   ```
   mkdir -p ~/.local/share/distroshelf/distrobox-1.7.2
   ln -sfn ~/.local/share/distroshelf/distrobox-bundled/distrobox-init \
           ~/.local/share/distroshelf/distrobox-1.7.2/distrobox-init
   # for every other distrobox-* entry in the current bundle dir:
   ln -sfn ~/.local/share/distroshelf/distrobox-bundled/distrobox-export \
           ~/.local/share/distroshelf/distrobox-1.7.2/distrobox-export
   ln -sfn ~/.local/share/distroshelf/distrobox-bundled/distrobox-host-exec \
           ~/.local/share/distroshelf/distrobox-1.7.2/distrobox-host-exec
   ```

   The entrypoint's source is the canonical trigger (its destination is the
   unique `/usr/bin/entrypoint`), so detection only needs to inspect that one
   mount. The entrypoint itself is **mandatory**: if its symlink does not
   resolve after creation, the migration fails (the bundle is broken and must
   be re-provisioned). The `distrobox-*` prefix is what scopes the sibling
   repair: it covers every bind-mounted script while excluding both the
   `distrobox` binary (no trailing hyphen) and the bundled install's
   `VERSION` marker (DistroShelf-internal, never bind-mounted into
   containers). The directory listing is the existence proof, so siblings
   that are absent from a partial bundle are simply not linked — no
   per-sibling `test -e` is needed.

5. **If the paths match but the file is missing**, this is not a migration
   problem — the init script is absent from its canonical location.  Treat
   this as a provisioning failure (re‑run `ProvisionScripts()` or
   re‑download the bundle) rather than silently creating a self‑referencing
   symlink.

All filesystem operations must go through `CommandRunner` (i.e. `mkdir -p`,
`ln -s`, `podman inspect`, `test -f`).  Because DistroShelf runs inside a
Flatpak sandbox, calling Rust's `std::fs::symlink` or `std::fs::create_dir_all`
directly operates on the sandbox filesystem — not the host — and has no effect
on the paths Podman sees.  The existing `distrobox_downloader.rs` module
already uses `CommandRunner` for all host‑side filesystem work and serves as
the reference pattern.

This approach is:

| Property          | Assessment |
|-------------------|------------|
| Non‑destructive   | Only creates symlinks; never touches container config or filesystem |
| Idempotent        | Re‑running the check on already‑ported containers is a no‑op |
| No data loss risk | Containers are never deleted or recreated |
| Simple to reason about | One filesystem operation per container |

## Architecture integration

The migration should be implemented within DistroShelf's existing patterns:

- **Detection**: A new method on `RootStore` — e.g. `check_stale_containers()`
  — runs the inspect‑and‑compare logic and returns the set of container names
  that need porting.  Expose the result as a bindable property (using `TypedListStore<T>`)
  so the UI can render a "N containers need migration" warning.

- **Execution**: The actual symlink creation is a `DistroboxTask` (per‑container
  or batched), so the user sees progress in the task manager.  The task body
  shells out through `CommandRunner` for `podman inspect`, `mkdir -p`, and
  `ln -s`.

- **Trigger points**: The check runs automatically when:

  | Event | Rationale |
  |-------|-----------|
  | Bundled distrobox download completes | Old versioned paths may now be stale |
  | User changes `distrobox-executable` setting | Source switch (bundled ↔ host) |
  | Application startup | Catch containers broken since last session |

- **UI**: A banner or infobar in the main window when stale containers are
  detected, with a one‑click "Migrate Containers" button that spawns the
  migration task.

### `podman inspect` output format

The `--volume` bind‑mounts live under the `Mounts` array in `podman inspect`
output.  The entry to locate has `"Destination": "/usr/bin/entrypoint"`, and
we extract `"Source"`:

```
podman inspect --format '{{ json .Mounts }}' <container-name>
```

Example output (trimmed):

```json
[{"Type":"bind","Source":"/home/user/.local/share/distroshelf/distrobox-1.7.2/distrobox-init","Destination":"/usr/bin/entrypoint","Mode":"ro","RW":false,"Propagation":"rprivate"}]
```

If the Mounts array is absent, empty, or contains no entry with
`Destination == "/usr/bin/entrypoint"`, the container was not created by
distrobox (or predates the current mount format).  Log a warning and skip
that container rather than panicking.

## Caveats

### Entrypoint argument compatibility

The container's entrypoint **arguments** (the `CMD` — e.g. `--init`, `--nvidia`,
`--additional-packages`, …) were set at creation time and are **not** updated.
If a new `distrobox-init` version changes its argument handling, old containers
carrying old arguments could break.  This is unlikely between minor releases,
but should be monitored during major version bumps.

### The `.containersetupdone` marker

`distrobox-init` uses the file `/.containersetupdone` inside the container
to skip re‑initialisation after the first run.  If a new init version adds
setup steps, old containers with the marker present will skip them.  This
is the same behaviour as if the host distrobox had been upgraded by a system
package manager.

### Root‑owned system paths cannot be symlinked by an unprivileged user

When a container was created with the host distrobox at `/usr/bin/distrobox-init`
and the user later switches to the bundled distrobox, the stale path lives in a
root‑owned directory.  A non‑root user cannot `mkdir -p /usr/bin/` nor place a
symlink there.

In this scenario the migration should fall back to one of:

1. Attempt `pkexec ln -s …` (if a polkit agent is available).
2. Re‑provision the host's `distrobox-init` at the system path by calling
   `distrobox enter` once with the host binary, which triggers
   `ProvisionScripts()` and writes the current init to the system location.
3. Warn the user and suggest installing the system distrobox package so the
   path remains valid.

### Host path must be a real directory, not a symlink

`os.Executable()` resolves the distrobox binary's path through
`/proc/self/exe`, which follows symlinks.  If the `distrobox-bundled/`
directory were itself a symlink (e.g. `distrobox-bundled → distrobox-1.8.0`),
**new** containers would get the real (versioned) path baked in, defeating
the stable‑path strategy.  DistroShelf's downloader already avoids this by
extracting to a temporary location and renaming to a real directory.
