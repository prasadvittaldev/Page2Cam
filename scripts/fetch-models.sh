#!/usr/bin/env bash
# Download the cut-out models and ONNX Runtime libraries into third_party/.
#   third_party/models/{rvm_mobilenetv3_fp32.onnx,selfie_landscape.onnx,test-person.png}
#   third_party/onnxruntime/linux/libonnxruntime.so
#   third_party/onnxruntime/windows/{onnxruntime.dll,DirectML.dll}
# Every download is pinned by sha256. Downloads are cached in ~/.cache/page2cam.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/page2cam/downloads"
MODELS="$ROOT/third_party/models"
ORT="$ROOT/third_party/onnxruntime"
mkdir -p "$CACHE" "$MODELS" "$ORT/linux" "$ORT/windows"

# ONNX Runtime 1.24.4: the newest release that also has a DirectML build
# (Microsoft.ML.OnnxRuntime.DirectML stops at 1.24.x). The app uses ort's api-24.
ORT_VER=1.24.4
HF_REV=2497d5bec26c626c7b3c4edc6e1fefc21b64f6c3

# name|url|sha256
FILES=(
  "rvm_mobilenetv3_fp32.onnx|https://github.com/PeterL1n/RobustVideoMatting/releases/download/v1.0.0/rvm_mobilenetv3_fp32.onnx|88d4531297118f595bf2fd60f6f566aec2e559393802d1f436c380f0cbbd2828"
  "selfie_landscape.onnx|https://huggingface.co/onnx-community/mediapipe_selfie_segmentation_landscape/resolve/$HF_REV/onnx/model.onnx|7a0adcfdb1715d3b0ff0f61486d8a181c33f4a208343a5abfccbc6540e872d24"
  "test-person.png|https://huggingface.co/datasets/Xenova/transformers.js-docs/resolve/main/selfie_segmentation_landscape.png|7b50f5ca48025bf0c5ea2d982b88f2ff2ecdf30c2529bf8c0ebaa4b29a95dc3a"
  "ort-$ORT_VER.nupkg|https://api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime/$ORT_VER/microsoft.ml.onnxruntime.$ORT_VER.nupkg|4b978d5065b85e7004b6c6f60ca494bd978fbe6836cbf0a0b52d82b61ab99638"
  "ort-directml-$ORT_VER.nupkg|https://api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime.directml/$ORT_VER/microsoft.ml.onnxruntime.directml.$ORT_VER.nupkg|57e9f11b73437bef7a309496135d4c1f96b1a8e9ddba60013fa27bfc1d788681"
  "vc_redist.x64.exe|https://aka.ms/vs/17/release/vc_redist.x64.exe|cc0ff0eb1dc3f5188ae6300faef32bf5beeba4bdd6e8e445a9184072096b713b"
  "directml-1.15.4.nupkg|https://api.nuget.org/v3-flatcontainer/microsoft.ai.directml/1.15.4/microsoft.ai.directml.1.15.4.nupkg|4e7cb7ddce8cf837a7a75dc029209b520ca0101470fcdf275c1f49736a3615b9"
)

fetch() {
  local name="$1" url="$2" want="$3" f="$CACHE/$1"
  if [ ! -f "$f" ]; then
    echo "  downloading $name"
    curl -fL --retry 3 -o "$f.part" "$url"
    mv "$f.part" "$f"
  fi
  local got
  got="$(sha256sum "$f" | cut -d' ' -f1)"
  if [ "${want#SHA_}" != "$want" ]; then
    echo "  UNPINNED $name sha256=$got" >&2
  elif [ "$got" != "$want" ]; then
    echo "checksum mismatch for $name (want $want, got $got)" >&2
    rm -f "$f"
    exit 1
  fi
}

echo "==> Fetching cut-out models and ONNX Runtime"
for e in "${FILES[@]}"; do
  IFS='|' read -r name url sha <<<"$e"
  fetch "$name" "$url" "$sha"
done

cp "$CACHE/rvm_mobilenetv3_fp32.onnx" "$CACHE/selfie_landscape.onnx" "$CACHE/test-person.png" "$MODELS/"
unzip -p "$CACHE/ort-$ORT_VER.nupkg" runtimes/linux-x64/native/libonnxruntime.so > "$ORT/linux/libonnxruntime.so"
unzip -p "$CACHE/ort-directml-$ORT_VER.nupkg" runtimes/win-x64/native/onnxruntime.dll > "$ORT/windows/onnxruntime.dll"
unzip -p "$CACHE/directml-1.15.4.nupkg" bin/x64-win/DirectML.dll > "$ORT/windows/DirectML.dll"
# onnxruntime.dll needs the Visual C++ 2015-2022 runtime; install-camera.cmd
# installs it. (aka.ms always serves the latest build: if the checksum stops
# matching, check the new file and update the pin.)
cp "$CACHE/vc_redist.x64.exe" "$ORT/windows/"
mkdir -p "$ORT/windows/licenses"
unzip -p "$CACHE/directml-1.15.4.nupkg" LICENSE.txt > "$ORT/windows/licenses/DirectML-LICENSE.txt"
unzip -p "$CACHE/directml-1.15.4.nupkg" ThirdPartyNotices.txt > "$ORT/windows/licenses/DirectML-ThirdPartyNotices.txt"
unzip -p "$CACHE/ort-directml-$ORT_VER.nupkg" LICENSE > "$ORT/windows/licenses/onnxruntime-LICENSE.txt"
unzip -p "$CACHE/ort-directml-$ORT_VER.nupkg" ThirdPartyNotices.txt > "$ORT/windows/licenses/onnxruntime-ThirdPartyNotices.txt"
for f in "$ORT/linux/libonnxruntime.so" "$ORT/windows/onnxruntime.dll" "$ORT/windows/DirectML.dll"; do
  [ -s "$f" ] || { echo "extracting $f failed" >&2; exit 1; }
done
echo "==> Done: $MODELS, $ORT"
