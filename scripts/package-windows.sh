#!/usr/bin/env bash
# Build the Windows 11 package: dist/Page2Cam-windows-x64.zip, the installer
# and SHA256SUMS(.sig). Runs on Linux (cargo-xwin cross-build) or natively on
# Windows in Git Bash: MSVC toolchain, CMake/Ninja/rc.exe on PATH, CEF in
# %LOCALAPPDATA%\cef-win, and NSIS 3 with the amd64-unicode stubs/plugins
# (official Windows NSIS + Debian's nsis-common files) at $MAKENSIS.
# Linux needs: cargo-xwin, clang-cl + lld-link on PATH, Windows CEF in $CEF_WIN,
# (export-cef-dir --target x86_64-pc-windows-msvc), llvm-rc, curl, zip, unzip.
# Cut-out models and ONNX Runtime come from scripts/fetch-models.sh.
set -euo pipefail

# Phases: "build" (compile and assemble dist/Page2Cam-windows-x64), "pack"
# (zip, installer, checksums from that folder; CI signs page2cam.exe and the
# camera DLL in between), or "all" (default: both).
PHASE="${1:-all}"
case "$PHASE" in all | build | pack) ;; *) echo "usage: $0 [all|build|pack]" >&2; exit 2 ;; esac

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
case "$(uname -s)" in MINGW* | MSYS* | CYGWIN*) NATIVE=1 ;; *) NATIVE= ;; esac
if [ -n "$NATIVE" ]; then
  CEF_WIN="${CEF_WIN:-$LOCALAPPDATA/cef-win}"
  MAKENSIS="${MAKENSIS:-$LOCALAPPDATA/page2cam-build/nsis/makensis.exe}"
else
  CEF_WIN="${CEF_WIN:-$HOME/.local/share/cef-win}"
fi
TARGET=x86_64-pc-windows-msvc
OUT="$ROOT/dist/Page2Cam-windows-x64"
ZIP="$ROOT/dist/Page2Cam-windows-x64.zip"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"
SETUP="$ROOT/dist/Page2Cam-Setup-$VERSION.exe"
STAGE="$ROOT/dist/installer-root"
NSIS_HOME="${NSIS_HOME:-$HOME/.local/share/nsis-pkg/root}"
RELEASE_KEY="${RELEASE_KEY:-$HOME/.config/page2cam/release-signing.key}"

# Pinned FFmpeg static build (GPL) from BtbN/FFmpeg-Builds.
FFMPEG_ZIP_NAME="ffmpeg-n8.1-latest-win64-gpl-8.1.zip"
FFMPEG_URL="https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/$FFMPEG_ZIP_NAME"
FFMPEG_SUMS_URL="https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/checksums.sha256"
FFMPEG_CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/page2cam/ffmpeg-win"

export PATH="$HOME/.local/bin:$PATH"
export CEF_PATH="$CEF_WIN"

if [ "$PHASE" != pack ]; then
echo "==> Building release binaries for $TARGET"
cd "$ROOT"
if [ -n "$NATIVE" ]; then
  cargo build --release -p page2cam -p p2c-vcam-win-source
  BIN="$ROOT/target/release"
else
  cargo xwin build --release -p page2cam -p p2c-vcam-win-source --target "$TARGET"
  BIN="$ROOT/target/$TARGET/release"
fi

echo "==> Assembling $OUT"
rm -rf "$OUT" "$ZIP" "$STAGE" "$ROOT"/dist/Page2Cam-Setup-*.exe "$ROOT"/dist/SHA256SUMS*
mkdir -p "$OUT"
cp "$BIN/page2cam.exe" "$BIN/p2c_vcam_win_source.dll" "$OUT/"
for f in libcef.dll chrome_elf.dll d3dcompiler_47.dll dxcompiler.dll vk_swiftshader.dll vk_swiftshader_icd.json \
         vulkan-1.dll v8_context_snapshot.bin icudtl.dat resources.pak chrome_100_percent.pak chrome_200_percent.pak \
         CREDITS.html; do
  cp "$CEF_WIN/$f" "$OUT/"
done
cp -r "$CEF_WIN/locales" "$OUT/"
for f in install-camera.cmd uninstall-camera.cmd README.txt page2cam.toml; do cp "$ROOT/packaging/windows/$f" "$OUT/"; done

echo "==> Cut-out models and ONNX Runtime"
"$ROOT/scripts/fetch-models.sh"
# The app looks for the runtime next to the exe and the models in models\.
cp "$ROOT/third_party/onnxruntime/windows/onnxruntime.dll" "$ROOT/third_party/onnxruntime/windows/DirectML.dll" \
   "$ROOT/third_party/onnxruntime/windows/vc_redist.x64.exe" "$OUT/"
mkdir -p "$OUT/models" "$OUT/licenses"
cp "$ROOT/third_party/models/rvm_mobilenetv3_fp32.onnx" "$ROOT/third_party/models/selfie_landscape.onnx" "$OUT/models/"
cp "$ROOT"/third_party/onnxruntime/windows/licenses/* "$OUT/licenses/"
cp "$ROOT/LICENSE" "$OUT/LICENSE.txt"
cp "$ROOT/packaging/THIRD_PARTY.txt" "$OUT/"

# Windows line endings for the scripts and text files.
for f in "$OUT"/*.cmd "$OUT"/*.txt "$OUT"/*.toml "$OUT"/licenses/*.txt; do sed -i 's/\r\?$/\r/' "$f"; done

echo "==> FFmpeg"
if [ ! -f "$FFMPEG_CACHE/ffmpeg.exe" ]; then
  tmp="$(mktemp -d)"
  curl -fL --retry 3 -o "$tmp/$FFMPEG_ZIP_NAME" "$FFMPEG_URL"
  curl -fL --retry 3 -o "$tmp/checksums.sha256" "$FFMPEG_SUMS_URL"
  want="$(grep " $FFMPEG_ZIP_NAME\$" "$tmp/checksums.sha256" | cut -d' ' -f1)"
  got="$(sha256sum "$tmp/$FFMPEG_ZIP_NAME" | cut -d' ' -f1)"
  if [ -z "$want" ] || [ "$want" != "$got" ]; then
    echo "FFmpeg checksum mismatch (want '$want', got '$got')" >&2
    exit 1
  fi
  mkdir -p "$FFMPEG_CACHE"
  unzip -j -o "$tmp/$FFMPEG_ZIP_NAME" '*/bin/ffmpeg.exe' -d "$FFMPEG_CACHE" >/dev/null
  rm -rf "$tmp"
fi
cp "$FFMPEG_CACHE/ffmpeg.exe" "$OUT/"

echo "==> Checking"
# Everything the cut-out needs at run time must be in the folder.
for f in page2cam.exe onnxruntime.dll DirectML.dll vc_redist.x64.exe models/rvm_mobilenetv3_fp32.onnx models/selfie_landscape.onnx; do
  [ -s "$OUT/$f" ] || { echo "missing from package: $f" >&2; exit 1; }
done
# onnxruntime.dll is loaded at run time, never linked.
if ! command -v llvm-readobj >/dev/null; then
  echo "(llvm-readobj not found: import check skipped)"
elif llvm-readobj --coff-imports "$OUT/page2cam.exe" | grep -qi onnxruntime; then
  echo "page2cam.exe must not import onnxruntime.dll" >&2; exit 1
fi

fi # build

if [ "$PHASE" = build ]; then
  echo "==> Assembled $OUT (next: sign, then $0 pack)"
  exit 0
fi
[ -s "$OUT/page2cam.exe" ] || { echo "nothing assembled at $OUT: run $0 build first" >&2; exit 1; }
rm -rf "$ZIP" "$STAGE" "$ROOT"/dist/Page2Cam-Setup-*.exe "$ROOT"/dist/SHA256SUMS*

echo "==> Zipping"
if [ -n "$NATIVE" ]; then
  # Windows' own tar (bsdtar) writes zip files.
  (cd "$ROOT/dist" && "$SYSTEMROOT/System32/tar.exe" -a -c -f "$(basename "$ZIP")" "$(basename "$OUT")")
else
  (cd "$ROOT/dist" && zip -qr -9 "$(basename "$ZIP")" "$(basename "$OUT")")
fi
ls -lh "$ZIP"

echo "==> Installer"
# Same files as the zip, minus the manual-install helpers (the installer does
# their job) and the settings file (installed copies keep settings in %APPDATA%).
# Icon and installer pictures (made by scripts/make-brand-assets.py).
BRAND="$ROOT/dist/brand"
mkdir -p "$STAGE" "$BRAND"
cp "$ROOT/crates/page2cam/windows/page2cam.ico" "$ROOT"/packaging/windows/installer-*.bmp "$BRAND/"
cp -r "$OUT"/. "$STAGE"/
rm -f "$STAGE"/install-camera.cmd "$STAGE"/uninstall-camera.cmd "$STAGE"/page2cam.toml \
      "$STAGE"/README.txt "$STAGE"/vc_redist.x64.exe
if [ -n "$NATIVE" ]; then
  w() { cygpath -w "$1"; }
  "$MAKENSIS" -V2 -WX \
    -DVERSION="$VERSION" -DSRC="$(w "$STAGE")" -DOUT="$(w "$SETUP")" \
    -DVCREDIST="$(w "$ROOT/third_party/onnxruntime/windows/vc_redist.x64.exe")" \
    -DBRAND="$(w "$BRAND")" \
    "$(w "$ROOT/packaging/windows/installer.nsi")"
else
  NSISDIR="$NSIS_HOME/usr/share/nsis" "$NSIS_HOME/usr/bin/makensis" -V2 -WX \
    -DVERSION="$VERSION" -DSRC="$STAGE" -DOUT="$SETUP" \
    -DVCREDIST="$ROOT/third_party/onnxruntime/windows/vc_redist.x64.exe" \
    -DBRAND="$BRAND" \
    "$ROOT/packaging/windows/installer.nsi"
fi
rm -rf "$STAGE" "$BRAND"
ls -lh "$SETUP"

echo "==> Checksums and signature (for the in-app updater)"
(cd "$ROOT/dist" && sha256sum "$(basename "$SETUP")" "$(basename "$ZIP")" > SHA256SUMS)
if [ -f "$RELEASE_KEY" ]; then
  cargo run -q -p p2c-update --example release_key -- sign "$RELEASE_KEY" "$ROOT/dist/SHA256SUMS"
else
  echo "WARNING: no release key at $RELEASE_KEY; SHA256SUMS is unsigned and the updater will refuse it" >&2
fi
ls -lh "$ROOT"/dist/SHA256SUMS*
