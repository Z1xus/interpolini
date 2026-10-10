# App menu on Linux

interpolini is a portable app, so it doesn't install anything on your system and it is not in the app menu by default. I don't plan to make a Flatpak, an AppImage or any other package for it, also because that would break the built-in updater.

But you can add it yourself with an icon and a desktop entry. First download the icon:

```sh
mkdir -p ~/.local/share/icons/hicolor/scalable/apps
curl -fsSLo ~/.local/share/icons/hicolor/scalable/apps/interpolini.svg https://raw.githubusercontent.com/Z1xus/interpolini/main/assets/interpolini.svg
```

Then save this as `~/.local/share/applications/interpolini.desktop` and change the Exec line to where you unzipped the app:

```ini
[Desktop Entry]
Type=Application
Name=Interpolini
Exec=/path/to/interpolini/interpolini
Icon=interpolini
Categories=AudioVideo;Video;
StartupWMClass=interpolini
```

The file has to be named interpolini.desktop, otherwise the taskbar doesn't use the icon for the window.
