FROM vllm/vllm-omni:v0.16.0

WORKDIR /app

COPY requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt

COPY app.py .

ENTRYPOINT []

CMD ["python","app.py"]