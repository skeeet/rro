# HEIC interoperability fixture

`kvazaar-smooth.heic` is an independently encoded 64×48 RGB8 test image, generated on 2026-10-27 before replacing the temporary native codec backend. It contains no photograph, research dataset, or upstream ResizeRightOptics source.

Source pixels, in top-left row order, are exactly:

```text
R(x,y) = 32 + 2*x
G(x,y) = 48 + 2*y
B(x,y) = 64 + x + y
x = 0..63, y = 0..47
```

The temporary `rro` implementation encoded the opaque sRGB image with libheif 1.23.7 and Kvazaar 2.3.2 at quality 0.9, using its scale-1 cubic path. The file embeds an sRGB ICC profile and uses HEVC 4:2:0. It tests the replacement decoder against another encoder rather than merely against its own output.

SHA-256: `f81c1e66d3cd6ae468c4997c74ab927b3a4d7c22f28669bdcd2d379b3e4d8887`

The synthetic pixel pattern and this generated fixture are part of this project's MIT-licensed test materials. The native libraries used to create it are not product runtime dependencies. Optional larger real-world interoperability inputs remain external development data, selected through `RRO_EXTERNAL_HEIC`; they are not needed to run the product.

## Limited-range HEVC without container NCLX

`x265-limited-no-nclx.heic` is a 64×48 independently encoded synthetic HEVC image retained from the 2026-10-27 interoperability investigation. Its bitstream signals limited (`tv`) range and an SMPTE 170M matrix, while its HEIF container has no NCLX color property. The RGBA8 golden file `x265-limited-ffmpeg.png` was decoded using FFmpeg. Both files are synthetic project test materials, not an upstream ResizeRightOptics image or implementation source.

This pair catches a real interoperability defect: treating the absent container NCLX as full range washed out colors by approximately `RGB * 219/255 + 16`. The regression compares decoded code values to the independent golden image within two code values, preserving the bitstream's range before RGB conversion. FFmpeg is not a test or product runtime dependency because its golden pixels are stored here.

SHA-256:

- `x265-limited-no-nclx.heic`: `8a9fc9142288361464bb639cf79aa3a33d1ed6ab739accc28b87d8eacc64e835`
- `x265-limited-ffmpeg.png`: `ce3d3af42bfad11a1a6aad6409317e66d2ff398c337744f0010887bac576e9cd`
