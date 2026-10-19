# Install

Bombadil comes with its own copies of uv and just, so you don't need to
install anything else.

=== "Installer"

    Download the file for your system from the
    [latest release](https://github.com/marcelotrevisani/bombadil/releases/latest):

    | System | File |
    |---|---|
    | Debian, Ubuntu | `bombadil_<version>-1_amd64.deb` or `_arm64.deb` |
    | Fedora, RHEL, openSUSE | `bombadil-<version>-1.x86_64.rpm` or `.aarch64.rpm` |
    | macOS (Apple Silicon and Intel) | `bombadil-<version>-macos-universal.dmg` |
    | Windows | `bombadil-<version>-x86_64-windows-setup.exe` |

    Install a `.deb` with `sudo apt install ./bombadil_*.deb` and an `.rpm`
    with `sudo dnf install ./bombadil-*.rpm`. On macOS, open the `.dmg` and
    drag Bombadil into Applications. The Windows setup installs for your user
    only unless you choose otherwise, and needs no administrator rights.

    Each release also has the bare binary in a `.tar.gz` (or a `.zip` on
    Windows), and a `SHA256SUMS` file to check the downloads against.

=== "pip / uv"

    ```sh
    pip install bombadil
    # or
    uv tool install bombadil
    ```

    The wheel contains the compiled app and puts a `bombadil` command on your
    PATH. There is no Python code inside it.

=== "cargo"

    ```sh
    cargo install bombadil
    ```

    This needs Rust 1.88 or newer.

=== "From source"

    ```sh
    git clone https://github.com/marcelotrevisani/bombadil
    cd bombadil
    cargo run --release -p bombadil
    ```

    The build downloads the pinned uv release for your platform.

Then run:

```sh
bombadil
```

## Supported platforms

| Platform | Prebuilt wheel | `cargo install` |
|---|---|---|
| Linux x86-64 and ARM64 | yes | yes |
| macOS on Apple Silicon | yes | yes |
| macOS on Intel | no | yes (builds from source) |
| Windows x86-64 | yes | yes |

## First launch on macOS and Windows

The macOS app and the Windows installer are not signed with a paid
developer certificate, so the system asks before the first launch.

- **macOS:** if macOS says it cannot check the app for malicious software,
  open **System Settings › Privacy & Security** and click **Open Anyway**
  next to Bombadil. You only need to do this once.
- **Windows:** if SmartScreen shows "Windows protected your PC", click
  **More info**, then **Run anyway**.

## Linux desktop integration

Each time Bombadil starts on Linux, it writes its own desktop entry and icons
under `~/.local/share`. No root access is needed. This is what lets GNOME on
Wayland show the right icon in the dock and the window switcher. See
[Gotchas](gotchas.md#the-window-shows-a-generic-icon-on-linux).

## Updating

Upgrade the same way you installed it: install the newer package or run
the newer setup over the old one, or use `pip install -U bombadil`,
`uv tool upgrade bombadil` or `cargo install bombadil`. Upgrading keeps your
settings and projects, which live in the
[configuration file](configuration.md).
