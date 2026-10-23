#!/usr/bin/env bash
# Downloads the ONNX models People & pets, Documents and search use into assets/models (or $1),
# and checks their SHA-256. All are free for commercial use:
#   YuNet face detector   - OpenCV Zoo, MIT
#   SFace face recognizer - OpenCV Zoo, Apache-2.0
#   MobileNetV2 (ImageNet)- ONNX Model Zoo, Apache-2.0
#   CLIP ViT-B/32 image encoder (OpenAI, MIT; int8 export by Xenova) - for Documents and search
#   CLIP ViT-B/32 text encoder + tokenizer (same source)               - for search by description
set -euo pipefail
cd "$(dirname "$0")/.."
# SHA-256 of a file: shasum on macOS, sha256sum elsewhere (Git Bash on Windows has no shasum).
sha256() {
  if command -v shasum >/dev/null; then shasum -a 256 "$1"; else sha256sum "$1"; fi
}

DEST="${1:-assets/models}"
mkdir -p "$DEST"

fetch() {
  local name=$1 url=$2 sha=$3
  local out="$DEST/$name"
  if [ -f "$out" ] && [ "$(sha256 "$out" | cut -d' ' -f1)" = "$sha" ]; then
    echo "ok       $name"
    return
  fi
  echo "fetching $name"
  curl -sSfL -o "$out.part" "$url"
  local got
  got=$(sha256 "$out.part" | cut -d' ' -f1)
  if [ "$got" != "$sha" ]; then
    rm -f "$out.part"
    echo "checksum mismatch for $name (got $got)" >&2
    exit 1
  fi
  mv "$out.part" "$out"
}

fetch face_detection_yunet_2023mar.onnx \
  https://github.com/opencv/opencv_zoo/raw/main/models/face_detection_yunet/face_detection_yunet_2023mar.onnx \
  8f2383e4dd3cfbb4553ea8718107fc0423210dc964f9f4280604804ed2552fa4
fetch face_recognition_sface_2021dec.onnx \
  https://github.com/opencv/opencv_zoo/raw/main/models/face_recognition_sface/face_recognition_sface_2021dec.onnx \
  0ba9fbfa01b5270c96627c4ef784da859931e02f04419c829e83484087c34e79
fetch mobilenetv2-12.onnx \
  https://github.com/onnx/models/raw/main/validated/vision/classification/mobilenet/model/mobilenetv2-12.onnx \
  c0c3f76d93fa3fd6580652a45618618a220fced18babf65774ed169de0432ad5

fetch clip_vision_quantized.onnx \
  https://huggingface.co/Xenova/clip-vit-base-patch32/resolve/main/onnx/vision_model_quantized.onnx \
  583fd1110a514667812fee7d684952aaf82a99b959760c8d7dca7e0ab9839299

fetch clip_text_quantized.onnx \
  https://huggingface.co/Xenova/clip-vit-base-patch32/resolve/main/onnx/text_model_quantized.onnx \
  73baab855d406190da9faa498cfedf65f15cf309f4cc7385b7b032e6d08e5c3a
fetch clip_tokenizer.json \
  https://huggingface.co/Xenova/clip-vit-base-patch32/resolve/main/tokenizer.json \
  f7f3b7af117d467b58374797691a6438d3e6b9e9cef800dfd5dced7f697a90cd

echo "Models are in $DEST"
