# The tray

Bombadil puts an icon in the system tray (the menu bar on macOS, the
notification area on Windows).

## Closing the window

By default, closing the window keeps Bombadil running in the tray. To make
closing quit the app instead, go to
[Settings › Appearance](settings.md#appearance) and choose **Quits** under
**Closing the window**.

If your desktop has no tray, closing always quits, because a hidden window
would have no way back.

## The tray menu

- **Show Bombadil** opens the window again. Clicking the icon also opens it:
  a left click on Linux, a double click on Windows. On macOS, any click opens
  the menu.
- **One submenu per project.**
    - With one environment, it has **Open terminal** and **Open in PyCharm**.
    - With several environments, it has one submenu per environment, labelled
      with its folder name and Python version, with the default marked
      *(active)*. Each has **Open terminal** and **Open in PyCharm**.
- **Quit** quits Bombadil.

The menu follows your projects: add, remove or rename one and the menu
updates.

!!! note "GNOME"
    Stock GNOME has no tray. Install the *AppIndicator and KStatusNotifierItem
    Support* extension (many distributions, including Ubuntu, ship it already
    enabled) to get one.
