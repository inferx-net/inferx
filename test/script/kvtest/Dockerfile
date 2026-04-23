FROM vllm/vllm-openai:v0.12.0

# Dependencies for Revyzor connector
RUN pip install --no-cache-dir \
    cupy-cuda12x \
    nvidia-nvcomp-cu12

# Install connector
COPY revyzor_test_stub.py /app/revyzor_test_stub.py
ENV PYTHONPATH="/app:${PYTHONPATH}"