#!/usr/bin/env python3
"""Original-Python pixel oracle for complete image conversion (development only).
Requires Pillow and NumPy. Exercises PNG final quantization, premultiplied alpha,
white-background lossy output, JPEG/HEIC encode+decode, and EXIF/XMP orientations.
"""
import argparse
import hashlib
import json
import math
from pathlib import Path
from report_privacy import write_report
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
import validate as validation
import numpy as np
from PIL import Image as PILImage, ImageOps, PngImagePlugin

WORK = ROOT / "reports/work/image-oracle"
BINARY = ROOT / "target/release/rro"


def invoke(source, dest, *args):
    complete = subprocess.run([str(BINARY), str(source), str(dest), *args], capture_output=True, text=True, timeout=120)
    if complete.returncode:
        raise RuntimeError(complete.stderr.strip())
    if complete.stdout:
        raise AssertionError("successful conversion wrote unsolicited standard output")


def quantize(data):
    # NumPy uses round-to-even; acceptance permits one code value at ties.
    return np.rint(np.clip(data, 0, 1) * 255).astype(np.uint8)


def expected_rgba(pixels, scales, *, opaque=False, regularization=0, camera="gaussian"):
    values = pixels.astype(np.float64) / 255
    values[..., :3] *= values[..., 3:]
    result = validation.original(values, {"scale_y": scales[0], "scale_x": scales[1], "camera": camera, "sigma": .7, "regularization": regularization})
    alpha = np.clip(result[..., 3:], 0, 1)
    if opaque:
        rgb = result[..., :3] + 1 - alpha
        return quantize(rgb)
    rgb = np.divide(result[..., :3], alpha, out=np.zeros_like(result[..., :3]), where=alpha > 0)
    quantized = quantize(np.concatenate((rgb, alpha), axis=-1))
    quantized[..., :3] = np.where((quantized[..., 3:] == 0), 0, quantized[..., :3])
    return quantized


def metrics(actual, expected):
    if actual.shape != expected.shape:
        raise AssertionError(f"shape mismatch: {actual.shape} versus {expected.shape}")
    diff = actual.astype(float) - expected.astype(float)
    mse = float(np.mean(diff ** 2))
    return dict(max_code_difference=float(np.max(np.abs(diff))), rmse_codes=math.sqrt(mse),
                fraction_differing=float(np.mean(diff != 0)), psnr_db=10 * math.log10(255 ** 2 / mse) if mse else None)


def png_color_type(path):
    with path.open("rb") as stream:
        header = stream.read(26)
    if len(header) != 26 or header[:8] != b"\x89PNG\r\n\x1a\n" or header[12:16] != b"IHDR":
        raise AssertionError("output does not have a PNG IHDR")
    return header[25]


def alpha_presence_cases(rgb, opaque, alpha):
    gray = PILImage.fromarray(rgb[..., 0])
    indices = np.indices(rgb.shape[:2]).sum(axis=0).astype(np.uint8) % 8
    indexed = PILImage.fromarray(indices).convert("P")
    indexed.putpalette([value for index in range(256)
                        for value in (index, (index * 3) % 256, (index * 7) % 256)])
    transparency = bytes(0 if index == 3 else 128 if index == 4 else 255
                         for index in range(256))
    fixtures = [
        ("rgb-png", PILImage.fromarray(rgb), {}, False, False),
        ("gray-png", gray, {}, False, False),
        ("indexed-png", indexed, {}, False, False),
        ("gray-trns-png", gray, {"transparency": int(rgb[0, 0, 0])}, True, False),
        ("indexed-trns-png", indexed, {"transparency": transparency}, True, False),
        ("rgba-opaque-png", PILImage.fromarray(opaque), {}, True, False),
        ("rgba-transparent-png", PILImage.fromarray(alpha), {}, True, False),
        ("rgb-constant", PILImage.fromarray(rgb), {}, False, True),
    ]
    results = []
    for label, image, save_options, has_alpha, constant in fixtures:
        name = "alpha-" + label
        source = WORK / f"{name}-input.png"
        destination = WORK / f"{name}-output.png"
        image.save(source, **save_options)
        try:
            flags = ["--scale", "0.5", "--camera", "gaussian", "--force"]
            options = dict(scale_x=.5, scale_y=.5, camera="gaussian")
            if constant:
                flags += ["--padding", "constant", "--no-normalize"]
                options.update(padding="constant", normalize=False)
            invoke(source, destination, *flags)
            with PILImage.open(source) as decoded:
                pixels = np.asarray(decoded.convert("RGBA" if has_alpha else "RGB"))
            expected = (expected_rgba(pixels, [.5, .5]) if has_alpha else
                        quantize(validation.original(pixels.astype(np.float64) / 255, options)))
            with PILImage.open(destination) as decoded:
                actual = np.asarray(decoded.convert("RGBA" if has_alpha else "RGB"))
            score = metrics(actual, expected)
            color_type = png_color_type(destination)
            results.append(dict(name=name, passed=score["max_code_difference"] <= 1 and
                                color_type == (6 if has_alpha else 2),
                                png_color_type=color_type, **score))
        except Exception as error:
            results.append(dict(name=name, passed=False, error=str(error)))
    return results


def oriented_pixels(pixels, orientation):
    """Independent array-coordinate oracle for the eight TIFF transforms."""
    transforms = {
        1: lambda image: image,
        2: np.fliplr,
        3: lambda image: np.rot90(image, 2),
        4: np.flipud,
        5: lambda image: np.swapaxes(image, 0, 1),
        6: lambda image: np.rot90(image, -1),
        7: lambda image: np.flip(np.swapaxes(image, 0, 1), axis=(0, 1)),
        8: np.rot90,
    }
    return transforms[orientation](pixels)


def xmp_orientation_cases(rgb):
    fixtures = [(f"orientation-png-xmp-{value}", value, None, None)
                for value in range(1, 9)]
    fixtures += [(f"orientation-png-xmp-{value}-{axis}", value, None, axis)
                 for value in (6, 8) for axis in ("width", "height")]
    fixtures += [(f"orientation-png-exif-{value}-over-xmp-8", 8, value, None)
                 for value in (1, 6)]
    results = []
    for name, xmp_orientation, exif_orientation, axis in fixtures:
        source = WORK / f"{name}-input.png"
        destination = WORK / f"{name}-output.png"
        description = ('<rdf:Description xmlns:tiff="http://ns.adobe.com/tiff/1.0/" '
                       f'tiff:Orientation="{xmp_orientation}"/>' if xmp_orientation % 2 else
                       '<rdf:Description xmlns:tiff="http://ns.adobe.com/tiff/1.0/">'
                       f'<tiff:Orientation>{xmp_orientation}</tiff:Orientation></rdf:Description>')
        xmp = ('<x:xmpmeta xmlns:x="adobe:ns:meta/">'
               '<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">'
               f'{description}</rdf:RDF></x:xmpmeta>')
        metadata = PngImagePlugin.PngInfo()
        metadata.add_itxt("XML:com.adobe.xmp", xmp)
        save_options = dict(pnginfo=metadata)
        if exif_orientation is not None:
            exif = PILImage.Exif()
            exif[274] = exif_orientation
            save_options["exif"] = exif
        PILImage.fromarray(rgb).save(source, **save_options)
        before = hashlib.sha256(source.read_bytes()).hexdigest()
        try:
            expected = oriented_pixels(rgb, exif_orientation or xmp_orientation)
            flags = ["--scale", "1"]
            if axis is not None:
                height, width = expected.shape[:2]
                scale = 31 / (width if axis == "width" else height)
                output_width = 31 if axis == "width" else max(1, math.ceil(width * scale - 8 * math.ulp(width * scale)))
                output_height = 31 if axis == "height" else max(1, math.ceil(height * scale - 8 * math.ulp(height * scale)))
                expected = quantize(validation.original(expected.astype(np.float64) / 255,
                                    dict(scale_x=scale, scale_y=scale,
                                         width=output_width, height=output_height)))
                flags = [f"--{axis}", "31"]
            invoke(source, destination, *flags, "--force")
            with PILImage.open(destination) as image:
                actual = np.asarray(image.convert("RGB"))
                output_orientation = image.getexif().get(274)
                output_xmp_present = any(key in image.info for key in ("xmp", "XML:com.adobe.xmp"))
                output_size = list(image.size)
            score = metrics(actual, expected)
            color_type = png_color_type(destination)
            source_unchanged = hashlib.sha256(source.read_bytes()).hexdigest() == before
            results.append(dict(name=name, passed=score["max_code_difference"] <= 1 and
                                color_type == 2 and output_orientation in (None, 1) and
                                not output_xmp_present and source_unchanged,
                                png_color_type=color_type, output_orientation=output_orientation,
                                output_xmp_present=output_xmp_present, output_size=output_size,
                                source_unchanged=source_unchanged, **score))
        except Exception as error:
            results.append(dict(name=name, passed=False, error=str(error)))
    return results


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--skip-heic", action="store_true", help="record HEIC as unverified; useful during codec development")
    parser.add_argument("--reference-dir", type=Path, help="external pinned Python oracle directory")
    args = parser.parse_args()
    initial_binary_hash = hashlib.sha256(BINARY.read_bytes()).hexdigest()
    validation.configure_reference(args.reference_dir)
    WORK.mkdir(parents=True, exist_ok=True)
    results = []
    y, x = np.mgrid[:43, :61]
    rgb = np.stack((20 + x * 3, 30 + y * 4, 40 + x + y * 2), axis=-1).astype(np.uint8)
    opaque = np.concatenate((rgb, np.full((43,61,1),255,dtype=np.uint8)),axis=-1)
    alpha = opaque.copy()
    alpha[..., 3] = np.where(x < 20, 0, np.where(x < 40, 255, (x-40)*12)).astype(np.uint8)
    # Strong signed kernel ringing: saturated visible boundary next to hidden colors.
    alpha[..., :3] = np.where((x < 20)[...,None], np.array([255,0,255],dtype=np.uint8), np.array([0,220,40],dtype=np.uint8))
    for name, source in (("opaque", opaque), ("alpha-edge", alpha)):
        input_path = WORK / f"{name}-input.png"
        PILImage.fromarray(source).save(input_path)
        before = hashlib.sha256(input_path.read_bytes()).hexdigest()
        for label, scales in (("down", [.6,.75]),("up",[1.5,1.7]),("identity",[1.,1.])):
            dest = WORK / f"{name}-{label}.png"
            try:
                invoke(input_path, dest, "--scale-x",str(scales[1]),"--scale-y",str(scales[0]),"--camera","gaussian","--sigma","0.7","--force")
                actual=np.asarray(PILImage.open(dest).convert("RGBA"))
                expected=expected_rgba(source,scales)
                score=metrics(actual,expected)
                color_type=png_color_type(dest)
                results.append(dict(name=f"png-{name}-{label}",passed=score["max_code_difference"]<=1 and color_type==6,png_color_type=color_type,**score))
            except Exception as e:
                results.append(dict(name=f"png-{name}-{label}",passed=False,error=str(e)))
        for label, scales in (("cubic-down", [.5,.5]), ("cubic-up", [1.5,1.5])):
            dest = WORK / f"{name}-{label}.png"
            try:
                invoke(input_path, dest, "--scale",str(scales[0]),"--force")
                actual=np.asarray(PILImage.open(dest).convert("RGBA"))
                expected=expected_rgba(source,scales,camera="cubic")
                score=metrics(actual,expected)
                color_type=png_color_type(dest)
                results.append(dict(name=f"png-{name}-{label}",passed=score["max_code_difference"]<=1 and color_type==6,png_color_type=color_type,**score))
            except Exception as e:
                results.append(dict(name=f"png-{name}-{label}",passed=False,error=str(e)))
        assert before==hashlib.sha256(input_path.read_bytes()).hexdigest(),"source modified"
    # JPEG/HEIC both directions. Decode through the product to avoid relying on
    # an unrelated system HEIC plug-in; inspect the returned lossless PNG.
    smooth_input=WORK/"smooth-input.png"
    PILImage.fromarray(opaque).save(smooth_input)
    smooth_rgb=opaque[...,:3]
    for ext in ("jpg","heic"):
        if ext=="heic" and args.skip_heic:
            results.append(dict(name="heic-roundtrip",passed=None,status="unverified: explicitly skipped"))
            continue
        encoded=WORK/f"smooth.{ext}"
        decoded=WORK/f"smooth-from-{ext}.png"
        try:
            invoke(smooth_input,encoded,"--scale","1","--quality","0.9","--force")
            invoke(encoded,decoded,"--scale","1","--force")
            actual=np.asarray(PILImage.open(decoded).convert("RGBA"))
            score=metrics(actual[...,:3],smooth_rgb)
            color_type=png_color_type(decoded)
            results.append(dict(name=f"{ext}-roundtrip",passed=(score["psnr_db"] is None or score["psnr_db"]>=30) and bool(np.all(actual[...,3]==255)) and color_type==2,png_color_type=color_type,**score))
        except Exception as e:
            results.append(dict(name=f"{ext}-roundtrip",passed=False,error=str(e)))
    # White compositing checked against pre-encoding original Python. JPEG
    # introduces unavoidable loss, measured separately from numeric parity.
    alpha_input=WORK/"alpha-edge-input.png"
    for ext in ("jpg","heic"):
        if ext=="heic" and args.skip_heic:
            continue
        encoded=WORK/f"white-alpha.{ext}"
        decoded=WORK/f"white-alpha-{ext}.png"
        try:
            invoke(alpha_input,encoded,"--scale","1.5","--camera","gaussian","--quality","0.99","--force")
            invoke(encoded,decoded,"--scale","1","--force")
            actual=np.asarray(PILImage.open(decoded).convert("RGB"))
            expected=expected_rgba(alpha,[1.5,1.5],opaque=True)
            score=metrics(actual,expected)
            results.append(dict(name=f"{ext}-white-alpha",passed=score["psnr_db"] is None or score["psnr_db"]>=30,**score))
        except Exception as e:
            results.append(dict(name=f"{ext}-white-alpha",passed=False,error=str(e)))
    results.extend(alpha_presence_cases(rgb, opaque, alpha))
    for orientation in range(1,9):
        source=WORK/f"orientation-{orientation}.jpg"
        dest=WORK/f"orientation-{orientation}.png"
        exif=PILImage.Exif()
        exif[274]=orientation
        PILImage.fromarray(rgb).save(source,quality=95,subsampling=0,exif=exif)
        try:
            invoke(source,dest,"--scale","1","--force")
            expected=np.asarray(ImageOps.exif_transpose(PILImage.open(source)).convert("RGB"))
            actual=np.asarray(PILImage.open(dest).convert("RGB"))
            score=metrics(actual,expected)
            # Different JPEG decoders can differ by a few integer values; the
            # asymmetric fixture makes an orientation mistake far larger.
            results.append(dict(name=f"orientation-{orientation}",passed=score["max_code_difference"]<=3,**score))
        except Exception as e:
            results.append(dict(name=f"orientation-{orientation}",passed=False,error=str(e)))
    for orientation in range(1,9):
        source=WORK/f"orientation-png-{orientation}-input.png"
        dest=WORK/f"orientation-png-{orientation}-output.png"
        exif=PILImage.Exif()
        exif[274]=orientation
        PILImage.fromarray(rgb).save(source,exif=exif)
        try:
            invoke(source,dest,"--scale","1","--force")
            with PILImage.open(source) as image:
                expected=np.asarray(ImageOps.exif_transpose(image).convert("RGB"))
            with PILImage.open(dest) as image:
                actual=np.asarray(image.convert("RGB"))
                output_orientation=image.getexif().get(274)
            score=metrics(actual,expected)
            color_type=png_color_type(dest)
            results.append(dict(name=f"orientation-png-{orientation}",
                                passed=score["max_code_difference"]<=1 and color_type==2 and output_orientation in (None,1),
                                png_color_type=color_type,output_orientation=output_orientation,**score))
        except Exception as error:
            results.append(dict(name=f"orientation-png-{orientation}",passed=False,error=str(error)))
    results.extend(xmp_orientation_cases(rgb))
    report=dict(date_utc=time.strftime("%Y-%m-%dT%H:%M:%SZ",time.gmtime()),binary_sha256=initial_binary_hash,
                binary_unchanged_during_checks=hashlib.sha256(BINARY.read_bytes()).hexdigest()==initial_binary_hash,
                original_commit=validation.PIN,reference_sha256=validation.REFERENCE_HASHES,numpy=np.__version__,
                cases=results,count=len(results),passed=sum(r["passed"] is True for r in results),failed=sum(r["passed"] is False for r in results),
                unverified=sum(r["passed"] is None for r in results))
    write_report(ROOT/"reports/image-parity.json", report)
    print(f"Image pipeline: {report['passed']}/{len(results)} passed, {report['failed']} failed, {report['unverified']} unverified")
    for r in results:
        if r["passed"] is False:
            print(json.dumps(r))
    sys.exit(1 if report["failed"] or report["unverified"] or not report["binary_unchanged_during_checks"] else 0)

if __name__=="__main__":
    main()
