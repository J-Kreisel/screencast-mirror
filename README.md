# Screencast Mirror

A small Rust/GTK app that mirrors a window through the XDG ScreenCast portal
and PipeWire. No recording, audio, or input forwarding.

Click **Choose window…**, select a window in your desktop's sharing dialog, and
resize the mirror as needed. The app uses a dark theme and stays in a normal,
borderless window—not fullscreen.

Move the pointer to the top edge (or press **Esc**) to reveal the toolbar:

- **Change target…** opens the desktop picker again.
- **Fill / Fit** switches between filling the window with cropping and showing
  the entire target with letterboxing. Neither mode stretches the image.
- In **Fill**, the slider moves the crop left/right or up/down, depending on
  which axis is cropped. The midpoint centers the image.
- **Stop** ends sharing. The FPS counter shows frames received from the desktop.

The toolbar overlays the cast and retracts when the pointer leaves its area;
it does not resize the image.

## Requirements

Linux with PipeWire, `xdg-desktop-portal`, and a portal backend that supports
**window** capture (such as GNOME or KDE). Some backends only support entire
screens; those will not work. Your desktop controls the picker and permissions.
Hidden or minimized windows may be throttled by the compositor or source app.
Some Niri setups throttle off-screen targets to about 1 FPS. Keep the source
window visible for smooth motion; the ScreenCast portal cannot override this
throttling. Static targets also naturally produce fewer frames.

## Nix

Once published on GitHub, replace `OWNER` with the repository owner:

```sh
nix run github:OWNER/screencast-mirror
# Or install permanently:
nix profile install github:OWNER/screencast-mirror
```

The flake supports x86_64 and aarch64 Linux. It bundles the application's
libraries and GStreamer plugins, not your system's portal services.
Installation also adds a **Screencast Mirror** entry to your application launcher.

## Development

```sh
nix develop
cargo run
```

Without Nix, install Rust, pkg-config, GTK 4.8+ and GStreamer development libraries,
plus the GStreamer base, PipeWire (`pipewiresrc`), and GTK 4
(`gtk4paintablesink`) plugins. Then run `cargo run`.

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
nix build
```

## License

MIT. See [LICENSE](LICENSE).
