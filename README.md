# Aloft

A screensaver for Linux that plays the aerial videos you know from Apple TV and macOS. Made for Wayland desktops (KDE Plasma, GNOME, Sway, Hyprland, niri and others).

**Aloft has nothing to do with Apple.** It doesn't ship any videos. When you click Download, it fetches them from Apple's public servers, the same ones Apple TV uses. The videos belong to Apple.

## Why I wrote it

I had mpv and swayidle doing this before. It worked, but on my PC it held about 1.4 GB of the graphics card's memory, which I'd rather keep. Aloft decodes the videos on the Intel graphics built into the CPU, and the NVIDIA card only puts the finished frames on screen. That needs about 250 MB. I haven't tested it on AMD hardware so reports are welcome.

## How it works

- `aloft --background` runs after login and waits until you're idle. It asks the compositor (`ext-idle-notify-v1`, or GNOME's idle monitor), so a video playing in your browser keeps it from starting.
- Then it opens a fullscreen window on every monitor and plays random clips with GStreamer. Mouse or keyboard ends it.
- Decoding happens in hardware via VA-API. Frames go to GTK as dmabufs and are never copied through the CPU. With two GPUs, the integrated one decodes and hands the frames to the one running your desktop.
- `aloft` opens the settings. The two processes don't talk to each other directly; the service watches the config file.
- Apple has no 1440p versions, so Aloft converts the 4K clips once on your GPU if you choose 1440p.

## Install

**Arch Linux:** `cd packaging/arch && makepkg -si`

**AppImage:** grab it from the releases, `chmod +x`, run. Needs glibc 2.39 or newer (Ubuntu 24.04, Debian 13, Fedora 40) and the VA-API driver for your GPU (`intel-media-driver`, or Mesa for AMD). To build it yourself: `./build-appimage.sh` (needs podman or docker).

**From source:** Rust, GTK 4.14+, libadwaita 1.5+, GStreamer 1.24+ with the `va` plugin, then `cargo build --release`.

## Usage

Start Aloft, pick a quality, hit Download. From then on it starts by itself after login.

```
aloft               settings
aloft --start       start the screensaver now
aloft --stop        stop it
aloft --quit        stop the background service
aloft --download [--quality 1080p|1440p|4k] [--refresh]
```

If your desktop has no idle protocol, call it from your idle daemon instead, e.g. `swayidle timeout 300 'aloft --start' resume 'aloft --stop'`.

Settings live in `~/.config/aloft/config.json`, videos in `~/.local/share/aloft/`. `ALOFT_DEBUG=1` logs what it's doing, which helps if playback stutters or doesn't start.

## License

[CC BY-NC-SA 4.0](LICENSE). Use it, change it, pass it on. Don't sell it, and anything you build from it has to stay under the same license.
