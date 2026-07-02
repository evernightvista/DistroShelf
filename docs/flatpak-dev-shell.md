# Flatpak Development Shell

Enter an interactive shell inside the Flatpak SDK sandbox to run `meson`,
`cargo`, and other build commands against the same toolchain the Flatpak build
uses (Rust from `rust-stable`, `clang`/`mold` linker, VTE, etc.).

## Prerequisites

- **Native `flatpak-builder` binary** (e.g. `dnf install flatpak-builder` on
  Fedora). The Flatpak-packaged `org.flatpak.Builder` runs inside its own
  sandbox and cannot spawn the nested `bwrap` namespace that an interactive
  shell requires, so a native install is needed for `--build-shell`.

- **GNOME SDK and extensions** listed in the manifest. Install any missing ones
  once with:

  ```bash
  flatpak-builder --disable-rofiles-fuse --install-deps-only _build com.ranfdev.DistroShelf.json
  ```

> **Note** — every `flatpak-builder` invocation below passes
> `--disable-rofiles-fuse` to bypass the FUSE/copy-on-write build layer, which
> avoids FUSE-related issues and is harmless for dev work.

## 1. Build dependencies (one-time, or after manifest changes)

Build every module **up to** `distroshelf` (VTE and the runtime setup) but stop
before compiling the project itself:

```bash
flatpak-builder --disable-rofiles-fuse --stop-at=distroshelf --force-clean _build com.ranfdev.DistroShelf.json
```

`--force-clean` wipes the output directory first so you start fresh. Omit it on
later runs to reuse the cached VTE build.

## 2. Enter the development shell

### `--build-shell` (recommended)

Drops you into `/run/build/distroshelf`, the prepared build directory with
sources extracted and the build environment (`PATH`, env vars from
`build-options`) configured:

```bash
flatpak-builder --disable-rofiles-fuse --build-shell=distroshelf _build com.ranfdev.DistroShelf.json
```

#### Exposing the host home directory

`--build-shell` enters the **build sandbox**, whose permissions come from
`build-options.build-args` in the manifest — it does not accept `--filesystem`
on the command line. To access the host home folder (e.g. for git config,
credentials, or a shared cargo cache), keep `--filesystem=home` in `build-args`:

```json
"build-options" : {
    "build-args" : [
        "--share=network",
        "--filesystem=home"
    ],
    ...
}
```

The build sandbox's `$HOME` is build-managed, so reach the host home by its
absolute path (e.g. `/var/home/fedora`). Use `--filesystem=host` instead if you
need paths outside the home directory.

### `--run` (fallback)

If only the Flatpak-packaged `org.flatpak.Builder` is available, `--run` enters
the sandbox through the Flatpak session helper (no nested `bwrap`) and accepts
`--filesystem` directly on the command line:

```bash
flatpak run org.flatpak.Builder --disable-rofiles-fuse --run \
  --filesystem=$(pwd) \
  _build com.ranfdev.DistroShelf.json bash
```

## 3. Common commands inside the shell

From the build directory (`/run/build/distroshelf` for `--build-shell`):

```bash
# Lint / format / test (same as the pre-commit hook)
cargo fmt --all -- --check
cargo clippy --all-targets --all-features
cargo test --all-features

# Full meson build
meson setup _fpbuild --libdir=lib --prefix=/app
ninja -C _fpbuild
ninja -C _fpbuild install   # installs into the sandbox /app, not the host
```

## Notes

- `cargo` reads from the project's `target/` directory, so builds done here
  share the same target cache as native builds **only** if the toolchain triplet
  and flags match. The Flatpak sandbox uses `clang` + `mold` (set in the
  manifest), which differs from a default native setup. Clean `target/` if you
  see confusing linker errors after switching between the two.
