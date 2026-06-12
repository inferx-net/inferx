#!/usr/bin/env python3
import os
import sys
import tempfile
from pathlib import Path
from flask import Flask, request, Response

app = Flask(__name__)

# Import docling only after Flask app is defined
from docling.document_converter import DocumentConverter

print("DEBUG: Initializing DocumentConverter", file=sys.stderr)
converter = DocumentConverter()
print("DEBUG: DocumentConverter initialized", file=sys.stderr)


@app.route('/convert', methods=['POST'])
def convert():
    """Convert document file to markdown using local Docling."""
    print("DEBUG: Received conversion request", file=sys.stderr)
    
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
        print(f"DEBUG: Converting {tmp_path}", file=sys.stderr)
        result = converter.convert(tmp_path)
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
        }
    }


if __name__ == '__main__':
    print("DEBUG: Starting Flask app", file=sys.stderr)
    app.run(host='0.0.0.0', port=8000)
