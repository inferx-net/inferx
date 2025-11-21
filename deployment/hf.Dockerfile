FROM python:3.10-slim

# Install dependencies
RUN pip install --no-cache-dir huggingface_hub

# Create working directory
WORKDIR /app

# Environment variable: where to store HF cache (vLLM-style)
ENV HF_HUB_CACHE=/models/hub

# Copy script
COPY download.py /app/download.py

# Entry point: download the model
ENTRYPOINT ["python3", "/app/download.py"]
