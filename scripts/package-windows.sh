#!/usr/bin/env bash
# Build the Windows 11 test package from Linux: dist/Page2Cam-windows-x64.zip
# Needs: cargo-xwin, clang-cl + lld-link on PATH, Windows CEF in $CEF_WIN,
# (export-cef-dir --target x86_64-pc-windows-msvc), llvm-rc, curl, zip, unzip.
# Cut-out models and ONNX Runtime come from scripts/fetch-models.sh.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CEF_WIN="${CEF_WIN:-$HOME/.local/share/cef-win}"
TARGET=x86_64-pc-windows-msvc
OUT="$ROOT/dist/Page2Cam-windows-x64"
ZIP="$ROOT/dist/Page2Cam-windows-x64.zip"

# Pinned FFmpeg static build (GPL) from BtbN/FFmpeg-Builds.
FFMPEG_ZIP_NAME="ffmpeg-n8.1-latest-win64-gpl-8.1.zip"
FFMPEG_URL="https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/$FFMPEG_ZIP_NAME"
FFMPEG_SUMS_URL="https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/checksums.sha256"
FFMPEG_CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/page2cam/ffmpeg-win"

export PATH="$HOME/.local/bin:$PATH"
export CEF_PATH="$CEF_WIN"

echo "==> Building release binaries for $TARGET"
cd "$ROOT"
cargo xwin build --release -p page2cam -p p2c-vcam-win-source --target "$TARGET"
BIN="$ROOT/target/$TARGET/release"

echo "==> Assembling $OUT"
rm -rf "$OUT" "$ZIP"
mkdir -p "$OUT"
cp "$BIN/page2cam.exe" "$BIN/p2c_vcam_win_source.dll" "$OUT/"
for f in libcef.dll chrome_elf.dll d3dcompiler_47.dll dxcompiler.dll vk_swiftshader.dll vk_swiftshader_icd.json \
         vulkan-1.dll v8_context_snapshot.bin icudtl.dat resources.pak chrome_100_percent.pak chrome_200_percent.pak \
         CREDITS.html; do
  cp "$CEF_WIN/$f" "$OUT/"
done
cp -r "$CEF_WIN/locales" "$OUT/"
cp "$ROOT"/packaging/windows/* "$OUT/"

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
if llvm-readobj --coff-imports "$OUT/page2cam.exe" | grep -qi onnxruntime; then
  echo "page2cam.exe must not import onnxruntime.dll" >&2; exit 1
fi

echo "==> Zipping"
(cd "$ROOT/dist" && zip -qr -9 "$(basename "$ZIP")" "$(basename "$OUT")")
ls -lh "$ZIP"
