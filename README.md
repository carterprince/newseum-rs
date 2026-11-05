<p align="center">
  <img src="data/io.github.carterprince.Newseum.svg" width="128" height="128" alt="Newseum app icon">
</p>

# Newseum

A native RSS and Atom reader for Linux, built with Rust, GTK 4, and libadwaita.

All your feeds appear in one searchable list, newest first, with the article on
the right. Nothing is cached and there's no read/unread tracking: feeds are
fetched again each time you open or refresh the app.

## Features

- **Native reader:** articles are rendered as GTK widgets rather than in a web view.
  Links open in your browser.
- **Podcasts:** play episodes in the app, with seeking.
- **YouTube:** paste a channel link and it's saved as a feed of the channel's
  videos, without Shorts.
- **Feed icons:** uses the feed's own icon, or else its website's favicon.
- **Manage feeds in the app:** URLs are checked before they're saved.

## Install

You'll need Rust, plus the development packages for GTK 4 (4.20+), libadwaita
(1.8+), and GStreamer, along with the GStreamer base and good plugins.

```sh
./install.sh
```

This builds Newseum and installs the binary, launcher, and icon under `~/.local`.
Set `PREFIX` to install somewhere else.

## Feeds

Feeds are stored in `~/.config/newseum/feeds.csv` as `Name,URL` with no header row.

```csv
Example Blog,https://example.com/feed.xml
```

## Shortcuts

| Action | Shortcut |
| --- | --- |
| Refresh | <kbd>Ctrl</kbd>+<kbd>R</kbd> or <kbd>F5</kbd> |
| Open in browser | <kbd>Ctrl</kbd>+<kbd>O</kbd> |
| Manage feeds | <kbd>Ctrl</kbd>+<kbd>,</kbd> |
| Close window | <kbd>Ctrl</kbd>+<kbd>W</kbd> |
| Quit | <kbd>Ctrl</kbd>+<kbd>Q</kbd> |

## License

GPL-3.0. See [LICENSE](LICENSE).
