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

RUN pip install --no-cache-dir docling onnxruntime dspy-ai trafilatura markdownify beautifulsoup4 httpx


ENV PYTHONUNBUFFERED=1
ENV DOCLING_CACHE_DIR=/app/.cache

WORKDIR /app

RUN pip install --no-cache-dir flask python-multipart requests

COPY convert.py .

# Create cache directory and pre-download OCR/models at build time
RUN mkdir -p ${DOCLING_CACHE_DIR}/models

# Pre-download OCR models by running a warm-up script
COPY warmup.py .
RUN python warmup.py

EXPOSE 8000

CMD ["python", "convert.py"]
