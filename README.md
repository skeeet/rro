# ResizeRight Optics CLI

`rro` resizes PNG, JPEG, and HEIC/HEIF images using camera-derived kernels from
[ResizeRightOptics](https://github.com/assafshocher/ResizeRightOptics).

## Install

Extract `rro-0.1.0-darwin-arm64.tar.gz` and run inside the extracted directory:

```sh
sh ./install.sh
export PATH="$HOME/.local/bin:$PATH"
rro --version
```

Installs to `$HOME/.local/bin/rro`.

## Resize

```sh
rro input.heic output.png --scale 0.5
rro input.jpg output.heic --width 1600
rro input.png output.jpg --width 1600 --height 900
```

One width or height preserves aspect ratio; setting both permits distortion.
The destination extension selects the output format. Existing outputs require
`--force`; the input is preserved. Run `rro --help` for camera, quality, and other controls.

## Image and platform limits

- The primary still image is processed, with EXIF orientation applied once.
- Supported SDR images become 8-bit sRGB. PQ/HLG HDR is rejected.
- PNG keeps alpha only when the input has it. JPEG and HEIC composite transparency onto white.
- Animation, auxiliary HEIF data, and metadata preservation are outside this version.

The Mac release targets Apple Silicon and macOS 13 or newer. Runtime on macOS 13,
Intel, Windows, and Linux remains unverified. The executable is not Developer ID
signed or notarized, so macOS may block a downloaded copy.

## Details

- [Installation](docs/INSTALL.md)
- [Build from source and development checks](docs/REBUILD.md)
- [Numerical behavior and architecture](docs/PORTING.md)

## Attribution and license

The method is by **Assaf Shocher**:
[*A Unified Theory of Image Scaling: Point Spread Functions, Discrete Kernels,
and Scale-Spaces*](https://assafshocher.github.io/image-scaling/).
This Rust reimplementation uses the [MIT LICENSE](LICENSE).
[THIRD_PARTY_NOTICES](THIRD_PARTY_NOTICES) retains original attribution and dependency
terms; the binary archive includes dependency license texts.
