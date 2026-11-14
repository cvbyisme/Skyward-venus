#!/usr/bin/env bash
# Offline-only CE build. Does not access or reboot the phone.
set -euo pipefail
HERE="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
VENUS_BUILD_FLAVOR=ce bash "$HERE/build-keymaster41.sh"
