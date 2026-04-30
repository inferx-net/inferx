FROM vllm/vllm-openai:v0.19.1

# Dependencies for Revyzor connector
RUN pip install --no-cache-dir \
    cupy-cuda12x \
    nvidia-nvcomp-cu12

# Install production connector v1.1 (compiled binary, sent via email)
# v1.1 changes: cleaner nvCOMP fallback logging, V3 cache reuse
COPY revyzor_connector_inferx.cpython-312-x86_64-linux-gnu.so /app/revyzor_connector_inferx.cpython-312-x86_64-linux-gnu.so
ENV PYTHONPATH="/app:${PYTHONPATH}"