from flask import Flask, request
import os
from pathlib import Path
from docling.document_converter import DocumentConverter
import tempfile

app = Flask(__name__)

@app.route('/convert', methods=['POST'])
def convert():
    # Get API key from header
    api_key = request.headers.get('Authorization', '').replace('Bearer ', '')
    
    # Set environment variables
    os.environ['DOCLING_API_URL'] = 'https://model.inferx.net/funccall/tn-a3t79iogb2/endpoints/Qwen3.6-35B-A3B-FP8/v1'
    os.environ['DOCLING_API_KEY'] = api_key
    os.environ['DOCLING_API_MODEL'] = 'Qwen/Qwen3.6-35B-A3B-FP8'
    
    # Get file
    file = request.files.get('file')
    if not file:
        return {'error': 'No file'}, 400
    
    # Save to temp
    with tempfile.NamedTemporaryFile(delete=False, suffix='.pdf') as tmp:
        file.save(tmp.name)
        tmp_path = Path(tmp.name)
    
    try:
        converter = DocumentConverter()
        result = converter.convert(tmp_path)
        content = result.document.export_to_markdown()
        return content, {'Content-Type': 'text/markdown'}
    finally:
        tmp_path.unlink()

if __name__ == '__main__':
    app.run(host='0.0.0.0', port=8000)
