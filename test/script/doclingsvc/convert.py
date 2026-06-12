#!/usr/bin/env python3
import os
import sys
import tempfile
from pathlib import Path
from flask import Flask, request, Response

app = Flask(__name__)

# Set cache directory before importing docling
# Use a writable directory for model cache
os.environ["DOCLING_CACHE_DIR"] = "/app/.cache"

# API Key configuration
API_KEY = os.environ.get("API_KEY")
if not API_KEY:
    print("WARNING: API_KEY not set, API calls will fail with authorization error", file=sys.stderr)

# Import docling only after Flask app is defined
from docling.document_converter import DocumentConverter, PdfFormatOption
from docling.datamodel.pipeline_options import ThreadedPdfPipelineOptions
from docling.datamodel.accelerator_options import AcceleratorDevice, AcceleratorOptions
from docling.datamodel.settings import settings
from docling.datamodel.base_models import InputFormat
from docling.pipeline.threaded_standard_pdf_pipeline import ThreadedStandardPdfPipeline

def create_converter(do_ocr=False, do_table_structure=False, 
                     do_picture_classification=False, do_picture_description=False,
                     do_chart_extraction=False, do_code_enrichment=False,
                     do_formula_enrichment=False):
    """Create DocumentConverter with specified options."""
    pipeline_options = ThreadedPdfPipelineOptions(
        ocr_batch_size=64,
        layout_batch_size=64,
        table_batch_size=4,
        accelerator_options=AcceleratorOptions(
            device=AcceleratorDevice.CPU,
        ),
        do_ocr=do_ocr,
        do_table_structure=do_table_structure,
        do_picture_classification=do_picture_classification,
        do_picture_description=do_picture_description,
        do_chart_extraction=do_chart_extraction,
        do_code_enrichment=do_code_enrichment,
        do_formula_enrichment=do_formula_enrichment,
    )
    
    settings.perf.page_batch_size = 16
    
    return DocumentConverter(
        format_options={
            InputFormat.PDF: PdfFormatOption(
                pipeline_cls=ThreadedStandardPdfPipeline,
                pipeline_options=pipeline_options,
            )
        }
    )


@app.route('/convert', methods=['POST'])
def convert():
    """Convert document file to markdown using local Docling."""
    
    # Authenticate via API key in Authorization header
    auth_header = request.headers.get('Authorization', '')
    # Support both "Bearer <key>" and raw key formats
    api_key = auth_header.replace('Bearer ', '').strip()
    
    if not API_KEY:
        return {'error': 'API_KEY not configured on server'}, 500
    
    if not api_key or api_key != API_KEY:
        return {'error': 'Authorization failed'}, 401
    
    print("DEBUG: Received conversion request", file=sys.stderr)
    
    # Parse optional configuration from headers, JSON body, or form fields
    config = {}
    
    # Check headers first (e.g., X-Do-OCR, X-Do-Table-Structure)
    header_mapping = {
        'X-Do-Ocr': 'do_ocr',
        'X-Do-Table-Structure': 'do_table_structure',
        'X-Do-Picture-Classification': 'do_picture_classification',
        'X-Do-Picture-Description': 'do_picture_description',
        'X-Do-Chart-Extraction': 'do_chart_extraction',
        'X-Do-Code-Enrichment': 'do_code_enrichment',
        'X-Do-Formula-Enrichment': 'do_formula_enrichment',
    }
    
    for header_key, config_key in header_mapping.items():
        header_value = request.headers.get(header_key)
        if header_value is not None:
            config[config_key] = header_value.lower() == 'true'
    
    # Fall back to JSON body
    if not config and request.is_json and request.json:
        config = request.json
    
    # Fall back to form fields (check if form has data, not just existence)
    if not config and request.form and any(request.form.keys()):
        for key in header_mapping.values():
            if key in request.form:
                config[key] = request.form.get(key, '').lower() == 'true'
    
    # Get options with defaults (all False by default)
    do_ocr = config.get('do_ocr', False)
    do_table_structure = config.get('do_table_structure', False)
    do_picture_classification = config.get('do_picture_classification', False)
    do_picture_description = config.get('do_picture_description', False)
    do_chart_extraction = config.get('do_chart_extraction', False)
    do_code_enrichment = config.get('do_code_enrichment', False)
    do_formula_enrichment = config.get('do_formula_enrichment', False)
    
    print(f"DEBUG: Conversion config: {config}", file=sys.stderr)
    
    file = request.files.get('file')
    if not file or file.filename == '':
        return {'error': 'No file provided'}, 400
    
    print(f"DEBUG: Processing file: {file.filename}", file=sys.stderr)
    
    supported_ext = ['.pdf', '.docx', '.pptx', '.html', '.htm', '.md', '.txt']
    file_ext = Path(file.filename).suffix.lower()
    
    if file_ext not in supported_ext:
        return {'error': f'Unsupported file type: {file_ext}'}, 400
    
    # Save to temp file
    with tempfile.NamedTemporaryFile(delete=False, suffix=file_ext) as tmp:
        file.save(tmp.name)
        tmp_path = Path(tmp.name)
    
    try:
        # Create converter with specified options
        print(f"DEBUG: Creating converter with do_ocr={do_ocr}", file=sys.stderr)
        temp_converter = create_converter(
            do_ocr=do_ocr,
            do_table_structure=do_table_structure,
            do_picture_classification=do_picture_classification,
            do_picture_description=do_picture_description,
            do_chart_extraction=do_chart_extraction,
            do_code_enrichment=do_code_enrichment,
            do_formula_enrichment=do_formula_enrichment,
        )
        
        print(f"DEBUG: Converting {tmp_path}", file=sys.stderr)
        result = temp_converter.convert(tmp_path)
        content = result.document.export_to_markdown()
        print(f"DEBUG: Content length = {len(content)}", file=sys.stderr)
        
        return Response(
            content,
            mimetype='text/markdown',
            headers={'Content-Disposition': f'attachment; filename="{tmp_path.stem}.md"'}
        )
    finally:
        if tmp_path.exists():
            tmp_path.unlink()


@app.route('/health', methods=['GET'])
def health():
    return {'status': 'healthy'}


@app.route('/', methods=['GET'])
def index():
    return {
        'name': 'Docling PDF to Markdown API',
        'version': '1.0.0',
        'endpoints': {
            '/health': 'Health check',
            '/convert': 'Convert document to markdown (POST)'
        },
        'config_options': {
            'do_ocr': 'Enable OCR (default: false)',
            'do_table_structure': 'Enable table structure extraction (default: false)',
            'do_picture_classification': 'Enable picture classification (default: false)',
            'do_picture_description': 'Enable picture description (default: false)',
            'do_chart_extraction': 'Enable chart extraction (default: false)',
            'do_code_enrichment': 'Enable code enrichment (default: false)',
            'do_formula_enrichment': 'Enable formula enrichment (default: false)'
        }
    }


if __name__ == '__main__':
    print("DEBUG: Starting Flask app", file=sys.stderr)
    app.run(host='0.0.0.0', port=8000)
