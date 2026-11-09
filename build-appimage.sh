#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

ENGINE=$(command -v podman || command -v docker || true)
if [ -z "$ENGINE" ]; then
	echo "podman or docker is required" >&2
	exit 1
fi
mkdir -p build/out build/work
# Rootless podman already maps container root to us, docker needs a chown.
owner=()
case "$ENGINE" in *docker) owner=(-e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)") ;; esac
"$ENGINE" run --rm \
	-v "$PWD:/src:ro,z" \
	-v "$PWD/build/out:/out:z" \
	-v "$PWD/build/work:/work:z" \
	"${owner[@]}" \
	docker.io/library/ubuntu:24.04 \
	bash /src/packaging/appimage/build-in-container.sh
mv -f build/out/*.AppImage build/
ls -la build/*.AppImage
