FROM docling-pdf2md:latest

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
