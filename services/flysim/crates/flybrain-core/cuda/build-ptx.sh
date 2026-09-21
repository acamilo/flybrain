#!/usr/bin/env bash
# Compile cuda/lif.cu to cuda/lif.ptx for sm_75 (Turing, the Quadro RTX 4000 on the host).
#
# The PTX is committed, so neither `cargo build --features cuda` nor a run needs any of this. Run
# it after editing lif.cu.
#
# There is no nvcc on the WSL box and none can be installed without root. The pip wheel
# `nvidia-cuda-nvcc-cu12` ships ptxas and libnvvm but no nvcc frontend and no cicc, so it cannot
# compile CUDA C++ on its own. `nvidia-cuda-nvrtc-cu12` does: NVRTC is a complete CUDA compiler in
# a shared library, it takes the same `--fmad=false` and `--gpu-architecture` options, and
# cu2ptx.c drives it in about a hundred lines. Install it as the user with no host changes:
#
#   mkdir -p ~/cuda && cd ~/cuda
#   curl -sL https://pypi.org/pypi/nvidia-cuda-nvrtc-cu12/json -o nvrtc.json
#   # take the manylinux x86_64 wheel URL out of that, then
#   curl -sL "<url>" -o nvrtc.whl && unzip -oq nvrtc.whl
#
# which leaves ~/cuda/nvidia/cuda_nvrtc/lib/libnvrtc.so.12.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
arch="${ARCH:-compute_75}"
libnvrtc="${LIBNVRTC:-$HOME/cuda/nvidia/cuda_nvrtc/lib/libnvrtc.so.12}"

if [ ! -f "$libnvrtc" ]; then
  echo "build-ptx.sh: no NVRTC at $libnvrtc; see the header of this script" >&2
  exit 1
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
cc -O2 -o "$tmp/cu2ptx" "$here/cu2ptx.c" -ldl
LIBNVRTC="$libnvrtc" "$tmp/cu2ptx" "$here/lif.cu" "$tmp/lif.ptx" "$arch"

# The bit-exactness contract in one grep: a single contracted multiply-add anywhere in the tick
# would round once where the CPU rounds twice, and the spike threshold would eventually disagree.
if grep -q 'fma\.' "$tmp/lif.ptx"; then
  echo "build-ptx.sh: the emitted PTX contains an fma instruction; --fmad=false did not take" >&2
  grep -n 'fma\.' "$tmp/lif.ptx" >&2
  exit 1
fi
# Every membrane store must be a separately rounded f64 -> f32 conversion.
if ! grep -q 'cvt\.rn\.f32\.f64' "$tmp/lif.ptx"; then
  echo "build-ptx.sh: the emitted PTX has no f64 -> f32 rounding; that cannot be right" >&2
  exit 1
fi

cp "$tmp/lif.ptx" "$here/lif.ptx"
echo "build-ptx.sh: wrote lif.ptx for $arch, no fma contraction"
