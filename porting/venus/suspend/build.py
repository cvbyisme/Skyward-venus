#!/usr/bin/env python3
"""Build the Rust service or --probe client."""
from pathlib import Path
import runpy
runpy.run_path(str(Path(__file__).resolve().parent.parent / 'build-rust-helper.py'))['main']('network-suspend')
