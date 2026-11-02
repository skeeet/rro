#!/usr/bin/env python3
"""Reproduce randomized float64 camera/kernel-control parity against the pin.

This supplements validate.py's named fixtures with non-integer kernel table
settings, exercising the arbitrary-length Bluestein inverse transform.
The external, pinned Python oracle is selected by validate.py using
RRO_REFERENCE_DIR; no upstream Python library is included in this project.
"""
import json
from report_privacy import write_report
import warnings

import numpy as np
import validate as validation


def main():
    validation.WORK.mkdir(parents=True, exist_ok=True)
    rng = np.random.default_rng(1094)
    results = []
    for number in range(100):
        shape = tuple(int(rng.integers(1, 24)) for _ in range(2)) + (int(rng.integers(1, 6)),)
        pixels = rng.normal(size=shape)
        sx, sy = rng.uniform(.15, 2.5, 2)
        options = dict(
            scale_x=float(sx), scale_y=float(sy),
            support=float(rng.uniform(4, 65)), density=float(rng.uniform(1, 17)),
            period_factor=float(rng.uniform(2, 8)),
            camera=['cubic', 'gaussian', 'ideal-lowpass'][number % 3],
            padding=['reflect', 'symmetric', 'wrap', 'edge', 'constant'][number % 5],
            window=['hann', 'none', 'lanczos'][number % 3],
            table_interp=['linear', 'cubic'][number % 2],
            regularization=.01, rcond=1e-12, normalize=bool(number % 4),
        )
        with warnings.catch_warnings():
            warnings.simplefilter('ignore')
            expected = validation.original(pixels, options)
        actual, metadata = validation.rust(pixels, options, name='core-fuzz', threads=1)
        difference = actual - expected
        maximum = float(np.max(np.abs(difference)))
        rmse = float(np.sqrt(np.mean(difference * difference)))
        results.append(dict(number=number, input_shape=shape, output_shape=actual.shape,
                            options=options, max_abs_error=maximum, rmse=rmse,
                            passed=maximum <= 1e-9 and rmse <= 1e-10,
                            warnings=metadata.get('warnings', '')))
    report = dict(environment=validation.environment(), seed=1094,
                  tolerances=dict(max_abs_error=1e-9, rmse=1e-10),
                  passed=all(result['passed'] for result in results),
                  cases=len(results), worst_max_abs_error=max(r['max_abs_error'] for r in results),
                  worst_rmse=max(r['rmse'] for r in results), results=results)
    destination = validation.REPORTS / 'core-fuzz.json'
    write_report(destination, report)
    print(f"{len(results)} randomized cases; passed={report['passed']}; "
          f"worst max abs={report['worst_max_abs_error']:.6g}; {destination}")
    return 0 if report['passed'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
