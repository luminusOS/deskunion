#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

cd "${PROJECT_ROOT}"

ANDROID_HOME="${ANDROID_HOME:-/opt/android}"
ANDROID_SDKVER="${ANDROID_SDKVER:-35.0.0}"
ANDROID_NDKVER="${ANDROID_NDKVER:-29.0.14206865}"
MINI_STUDIO_DIR="${MINI_STUDIO_DIR:-/opt/mini-studio}"

if [ -f "./meson/meson.py" ]; then
    MESON_BIN="./meson/meson.py"
else
    MESON_BIN="$(which meson)"
fi

if [ -f "./pixiewood/pixiewood" ]; then
    PIXIEWOOD_BIN="./pixiewood/pixiewood"
else
    PIXIEWOOD_BIN="$(which pixiewood)"
fi

# Redirect gitlab.gnome.org/GNOME to official GitHub mirror to avoid Gitaly outages
git config --global url."https://github.com/GNOME/".insteadOf "https://gitlab.gnome.org/GNOME/" || true

echo "=== Preparing DeskUnion Android build with Pixiewood ==="
"${PIXIEWOOD_BIN}" prepare \
    -a "${MINI_STUDIO_DIR}" \
    --release \
    -s "${ANDROID_HOME}" \
    --meson "${MESON_BIN}" \
    build-aux/android/deskunion.xml

echo "=== Generating Android application skeleton ==="
"${PIXIEWOOD_BIN}" generate

echo "=== Building DeskUnion Android APK ==="
"${PIXIEWOOD_BIN}" build

mkdir -p apks/

# Ensure keystore exists for signing
KEYSTORE="build-aux/android/debug.keystore"
if [ ! -f "${KEYSTORE}" ]; then
    if [ -f "subprojects/gtk/.gitlab-ci/debug.keystore" ]; then
        KEYSTORE="subprojects/gtk/.gitlab-ci/debug.keystore"
    else
        echo "=== Generating debug keystore for APK signing ==="
        keytool -genkeypair -v \
            -keystore "${KEYSTORE}" \
            -storepass android \
            -alias androiddebugkey \
            -keypass android \
            -keyalg RSA \
            -keysize 2048 \
            -validity 10000 \
            -dname "CN=DeskUnion Debug,O=LuminusOS,C=US"
    fi
fi

APKSIGNER="${ANDROID_HOME}/build-tools/${ANDROID_SDKVER}/apksigner"

echo "=== Signing release APKs ==="
find .pixiewood/android/app/build/outputs/apk/release/ -type f -name '*.apk' | while read -r apk_file; do
    apk_basename="$(basename "${apk_file}")"
    signed_name="$(echo "${apk_basename}" | sed 's/unsigned/release-signed/g')"
    echo "Signing ${apk_basename} -> apks/${signed_name}"
    "${APKSIGNER}" sign \
        --ks "${KEYSTORE}" \
        --ks-pass "pass:android" \
        --in "${apk_file}" \
        --out "apks/${signed_name}"
done

echo "=== Finished! Signed APKs available in apks/ ==="
ls -lh apks/
