# PDF-to-Markdown container using Docling with LLM backend
# Input: /input/*.pdf, *.html, etc.  ->  Output: /output/merged.md
FROM python:3.11-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    gcc \
    libgl1 \
    libglib2.0-0t64 \
    libgbm1 \
    libpango-1.0-0 \
    libpangocairo-1.0-0 \
    libcairo2 \
    libgdk-pixbuf-xlib-2.0-0 \
    libffi-dev \
    fonts-dejavu-core \
    poppler-utils \
    && rm -rf /var/lib/apt/lists/* \
    && fc-cache -f

WORKDIR /app

RUN pip install --no-cache-dir docling onnxruntime dspy-ai trafilatura markdownify beautifulsoup4 httpx

COPY convert.py ./convert.py
COPY crawl2md.py ./crawl2md.py
COPY count_tokens.py ./count_tokens.py

RUN mkdir -p /input /output

RUN python -c "from rapidocr import RapidOCR; ocr = RapidOCR(); print('Pre-downloaded OCR models')" 2>&1

COPY test_dspy.py ./test_dspy.py

COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

ENTRYPOINT ["/entrypoint.sh"]
