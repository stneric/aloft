#!/usr/bin/env bash
# Runs inside the Ubuntu 24.04 container started by build-appimage.sh.
set -euo pipefail

SRC=/src
OUT=/out
WORK=/work
APPDIR=$WORK/AppDir
ARCH=x86_64
TRIPLET=x86_64-linux-gnu
export DEBIAN_FRONTEND=noninteractive
export APPIMAGE_EXTRACT_AND_RUN=1

echo ":: Installing build dependencies"
apt-get update -qq
apt-get install -y -qq --no-install-recommends \
	build-essential ca-certificates curl file pkg-config patchelf desktop-file-utils \
	libgtk-4-dev libadwaita-1-dev \
	libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev libgstreamer-plugins-bad1.0-dev \
	gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
	gstreamer1.0-gl gstreamer1.0-tools \
	librsvg2-common librsvg2-bin adwaita-icon-theme libglib2.0-bin >/dev/null

if ! command -v cargo >/dev/null; then
	echo ":: Installing Rust"
	curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal >/dev/null
fi
# shellcheck disable=SC1091
. "$HOME/.cargo/env"

echo ":: Building"
export CARGO_TARGET_DIR=$WORK/target
cargo build --release --locked --manifest-path "$SRC/Cargo.toml"

echo ":: Assembling AppDir"
rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/lib" "$APPDIR/usr/share/applications" \
	"$APPDIR/usr/share/icons/hicolor/scalable/apps" "$APPDIR/usr/share/metainfo" \
	"$APPDIR/usr/share/glib-2.0/schemas" "$APPDIR/apprun-hooks"
install -m755 "$CARGO_TARGET_DIR/release/aloft" "$APPDIR/usr/bin/aloft"
install -m644 "$SRC/data/de.eric_s.Aloft.desktop" "$APPDIR/usr/share/applications/"
install -m644 "$SRC/data/de.eric_s.Aloft.svg" "$APPDIR/usr/share/icons/hicolor/scalable/apps/"
install -m644 "$SRC/data/de.eric_s.Aloft.metainfo.xml" "$APPDIR/usr/share/metainfo/"

# Only the plugins Aloft uses. VA-API drivers come from the host.
GST_SRC=/usr/lib/$TRIPLET/gstreamer-1.0
GST_DST=$APPDIR/usr/lib/gstreamer-1.0
mkdir -p "$GST_DST"
for p in coreelements playback typefindfunctions isomp4 videoparsersbad va \
	videoconvertscale app opengl nvcodec; do
	if [ -f "$GST_SRC/libgst$p.so" ]; then
		cp "$GST_SRC/libgst$p.so" "$GST_DST/"
	else
		echo "   (plugin $p not available, skipped)"
	fi
done
mkdir -p "$APPDIR/usr/libexec/gstreamer-1.0"
cp /usr/lib/$TRIPLET/gstreamer1.0/gstreamer-1.0/gst-plugin-scanner "$APPDIR/usr/libexec/gstreamer-1.0/"

# GSettings schemas (GTK's file chooser needs them when no portal is around).
cp /usr/share/glib-2.0/schemas/org.gtk.gtk4.*.xml "$APPDIR/usr/share/glib-2.0/schemas/" 2>/dev/null || true
glib-compile-schemas "$APPDIR/usr/share/glib-2.0/schemas"

# SVG loader for gdk-pixbuf (icons); the cache gets its paths at runtime.
PIXBUF_SRC=$(pkg-config --variable=gdk_pixbuf_moduledir gdk-pixbuf-2.0)
PIXBUF_DST=$APPDIR/usr/lib/gdk-pixbuf-2.0/loaders
mkdir -p "$PIXBUF_DST"
cp "$PIXBUF_SRC"/libpixbufloader-svg.so "$PIXBUF_DST/" 2>/dev/null || true
cp "$PIXBUF_SRC"/libpixbufloader-png.so "$PIXBUF_DST/" 2>/dev/null || true
QUERY=$(command -v gdk-pixbuf-query-loaders || echo "/usr/lib/$TRIPLET/gdk-pixbuf-2.0/gdk-pixbuf-query-loaders")
GDK_PIXBUF_MODULEDIR=$PIXBUF_DST "$QUERY" "$PIXBUF_DST"/*.so |
	sed "s|$PIXBUF_DST|@APPDIR_LOADERS@|g" >"$APPDIR/usr/lib/gdk-pixbuf-2.0/loaders.cache.in"

# Symbolic icons, so buttons have icons whatever theme the host uses.
mkdir -p "$APPDIR/usr/share/icons/Adwaita"
cp -r /usr/share/icons/Adwaita/symbolic "$APPDIR/usr/share/icons/Adwaita/" 2>/dev/null || true
cp /usr/share/icons/Adwaita/index.theme "$APPDIR/usr/share/icons/Adwaita/"
cp -r /usr/share/icons/hicolor/index.theme "$APPDIR/usr/share/icons/hicolor/"

echo ":: Bundling libraries"
cd "$WORK"
if [ ! -x linuxdeploy-$ARCH.AppImage ]; then
	curl -sSLo linuxdeploy-$ARCH.AppImage \
		https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-$ARCH.AppImage
	chmod +x linuxdeploy-$ARCH.AppImage
fi
if [ ! -x appimagetool-$ARCH.AppImage ]; then
	curl -sSLo appimagetool-$ARCH.AppImage \
		https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-$ARCH.AppImage
	chmod +x appimagetool-$ARCH.AppImage
fi
deps=()
for f in "$GST_DST"/*.so "$PIXBUF_DST"/*.so "$APPDIR/usr/libexec/gstreamer-1.0/gst-plugin-scanner"; do
	deps+=(--deploy-deps-only "$f")
done
./linuxdeploy-$ARCH.AppImage --appdir "$APPDIR" \
	-e "$APPDIR/usr/bin/aloft" \
	-d "$APPDIR/usr/share/applications/de.eric_s.Aloft.desktop" \
	-i "$APPDIR/usr/share/icons/hicolor/scalable/apps/de.eric_s.Aloft.svg" \
	"${deps[@]}" >/dev/null

# The host's graphics drivers are built against its own copies of these.
echo ":: Removing host-provided libraries"
for pat in 'libstdc++.so*' 'libgcc_s.so*' 'libva.so*' 'libva-*.so*' 'libdrm.so*' 'libdrm_*.so*' \
	'libgbm.so*' 'libwayland-*.so*' 'libvulkan.so*' 'libEGL*.so*' 'libGL*.so*' 'libOpenGL.so*' \
	'libxcb*.so*' 'libX11*.so*' 'libxkbcommon*.so*' 'libudev.so*' 'libsystemd.so*' 'libdbus-1.so*'; do
	find "$APPDIR/usr/lib" -maxdepth 1 -name "$pat" -print -delete | sed 's|.*/|   - |'
done

rsvg-convert -w 256 -h 256 "$SRC/data/de.eric_s.Aloft.svg" -o "$APPDIR/de.eric_s.Aloft.png"
rm -f "$APPDIR/.DirIcon"
cp "$APPDIR/de.eric_s.Aloft.png" "$APPDIR/.DirIcon"
# linuxdeploy created AppRun as a symlink to the binary; replace the link,
# don't write through it.
rm -f "$APPDIR/AppRun"
cp "$SRC/packaging/appimage/AppRun" "$APPDIR/AppRun"
chmod +x "$APPDIR/AppRun"
rm -f "$APPDIR/apprun-hooks/"* 2>/dev/null || true

file "$APPDIR/usr/bin/aloft" | grep -q ELF || { echo "usr/bin/aloft is not an ELF binary" >&2; exit 1; }

echo ":: Creating AppImage"
VERSION=$(grep -m1 '^version' "$SRC/Cargo.toml" | cut -d'"' -f2)
mkdir -p "$OUT"
ARCH=$ARCH VERSION=$VERSION ./appimagetool-$ARCH.AppImage --no-appstream "$APPDIR" \
	"$OUT/Aloft-$VERSION-$ARCH.AppImage" >/dev/null
chown "${HOST_UID:-0}:${HOST_GID:-0}" "$OUT/Aloft-$VERSION-$ARCH.AppImage" 2>/dev/null || true
ls -la "$OUT/Aloft-$VERSION-$ARCH.AppImage"
