# Install

Bombadil bundles its own copy of uv, so you don't need to install anything
else.

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

## Linux desktop integration

Each time Bombadil starts on Linux, it writes its own desktop entry and icons
under `~/.local/share`. No root access is needed. This is what lets GNOME on
Wayland show the right icon in the dock and the window switcher. See
[Gotchas](gotchas.md#the-window-shows-a-generic-icon-on-linux).

## Updating

Upgrade the same way you installed it, with `pip install -U bombadil`,
`uv tool upgrade bombadil` or `cargo install bombadil`. Your settings and
projects are kept, because they live in the
[configuration file](configuration.md), not in the app.
