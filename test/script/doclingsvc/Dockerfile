FROM docling-pdf2md:latest

ENV PYTHONUNBUFFERED=1

WORKDIR /app

RUN pip install --no-cache-dir flask python-multipart requests

COPY convert.py .

EXPOSE 8000

CMD ["python", "convert.py"]
