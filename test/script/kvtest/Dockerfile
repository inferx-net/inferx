FROM vllm/vllm-openai:v0.12.0

# Ensure pip is up to date and install the specific CUDA 12 packages
# nvCOMP is used for high-speed compression (often for checkpoints/snapshots)
# cupy-cuda12x provides NumPy-like GPU acceleration for CUDA 12
RUN pip install --no-cache-dir \
    cupy-cuda12x \
    nvidia-nvcomp-cu12