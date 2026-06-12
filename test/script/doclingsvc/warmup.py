#!/usr/bin/env python3
"""Warmup script to pre-download OCR models at Docker build time."""

import sys
import os

# Set cache directory before importing docling
os.environ["DOCLING_CACHE_DIR"] = "/app/.cache"

from pathlib import Path
from docling.datamodel.pipeline_options import PdfPipelineOptions
from docling.datamodel.accelerator_options import AcceleratorOptions
from docling.document_converter import DocumentConverter, PdfFormatOption
from docling.datamodel.base_models import InputFormat

def warmup():
    """Initialize Docling converter to trigger model downloads."""
    print("Warmup: Pre-downloading models...", file=sys.stderr)
    
    # Configure pipeline options (CPU only, no unnecessary features)
    pipeline_options = PdfPipelineOptions(
        accelerator_options=AcceleratorOptions(
            device="cpu",
        ),
        do_ocr=True,           # Enable OCR to download OCR models
        do_table_structure=False,  # Disable table structure to speed up warmup
        do_picture_classification=False,
        do_picture_description=False,
        do_chart_extraction=False,
        do_code_enrichment=False,
        do_formula_enrichment=False,
    )
    
    converter = DocumentConverter(
        format_options={
            InputFormat.PDF: PdfFormatOption(
                pipeline_options=pipeline_options,
            )
        }
    )
    
    # Initialize the pipeline to trigger model downloads
    converter.initialize_pipeline(InputFormat.PDF)
    
    print("Warmup: Models downloaded successfully!", file=sys.stderr)
    print(f"Cache directory: {Path(os.environ.get('DOCLING_CACHE_DIR', Path.home() / '.cache' / 'docling'))}", file=sys.stderr)

if __name__ == "__main__":
    warmup()
