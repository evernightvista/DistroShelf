# Flatpak Development Shell

`./flatpak-dev-shell.sh` drops you into an interactive bash inside the Flatpak
SDK build sandbox — same toolchain the Flatpak build uses (Rust from
`rust-stable`, `clang`/`mold` linker, VTE) — with sources extracted and meson
already configured.

## Prerequisites

- **Native `flatpak-builder`** (e.g. `dnf install flatpak-builder`). The
  Flatpak-packaged `org.flatpak.Builder` runs in its own sandbox and can't spawn
  the nested `bwrap` namespace that an interactive shell needs.

- **Dependencies built once** (and again after manifest changes):
  ```bash
  flatpak-builder --disable-rofiles-fuse --stop-at=distroshelf --force-clean _build com.ranfdev.DistroShelf.json
  ```
  This builds VTE into `_build` and stops before compiling the project.

## Usage

```bash
./flatpak-dev-shell.sh
```

You land at `/run/build/distroshelf` (the project source root) with `cargo`,
`meson`, `ninja`, `clang`, and `mold` on `PATH`. meson is already configured in
`_flatpak_build/`, so iteration is fast:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features
cargo test --all-features

ninja -C _flatpak_build                 # rebuild
ninja -C _flatpak_build install         # install into the sandbox /app
```

To reach host files (e.g. a shared cargo cache, git credentials), use absolute
host paths — the home folder is exposed via `--filesystem=home` in
`build-options.build-args`, since the sandbox `$HOME` is build-managed.

## How the script works

`flatpak-builder --build-shell` has several non-obvious behaviors; the script
encodes workarounds for each.

**`--build-shell` prepares and runs meson.** It extracts the module's sources
into `/run/build/<module>`, runs the meson configure step, then launches `/bin/sh`
in the builddir (`_flatpak_build`). The script feeds two commands via stdin — a
`cd` to the source root, then `exec bash -i </dev/tty` — so you start at the
project root in a real interactive bash instead of inside `_flatpak_build`.

**`$SHELL` is ignored.** `--build-shell` always uses `/bin/sh` regardless of the
`$SHELL` env var, so the shell can't be swapped via the environment. Piping the
setup commands and reconnecting to the controlling terminal (`/dev/tty`) is what
makes the handoff work.

**`--disable-rofiles-fuse` is always passed.** rofiles-fuse is broken on this
host (mounts are sometimes not cleaned up and require a manual `umount`), so
every `flatpak-builder` call disables it. This is also why `--run` is avoided: it
forces rofiles-fuse and does not accept `--disable-rofiles-fuse`.

**Stale build dirs are pruned.** `--build-shell` re-extracts the `dir` source
into a fresh `<module>-N` directory on every run (the source is treated as
changed each time, so caching never applies). The stable `<module>` symlink
always points at the newest; the script deletes every other `<module>-N` dir,
since nothing reuses them.

**`_build` is the app prefix, not the meson build dir.** `_build` (the
`flatpak-builder` DIRECTORY argument) holds every previously-built module
installed into the app prefix and is mounted as `/app` in the sandbox — that's
how VTE becomes visible to the project build. `_flatpak_build` is only meson's
per-project output. The script refuses to run without `_build`, since the shell
is useless without the dependencies in `/app`.

## Notes

- `cargo` shares `target/` with native builds **only** if the toolchain triplet
  and flags match. The Flatpak sandbox uses `clang` + `mold` (set in the
  manifest), unlike a default native setup — clean `target/` if you see linker
  errors after switching between the two.
