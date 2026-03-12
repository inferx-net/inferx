# Use the official vLLM OpenAI image
FROM vllm/vllm-openai:v0.14.1

# 1. Install system dependencies for audio and git
RUN apt-get update && apt-get install -y --no-install-recommends \
    ffmpeg \
    libsndfile1 \
    git \
    && rm -rf /var/lib/apt/lists/*

# 2. Clone and install the VibeVoice plugin
# Installing the repo registers the 'vllm_plugin' module
RUN git clone --depth=1 https://github.com/microsoft/VibeVoice.git /vibevoice
RUN pip install --no-cache-dir '/vibevoice'

# 3. Bake the Tokenizer into the image
# This creates a local directory with the 'Big Five' files (vocab.json, etc.)
# needed to avoid the 'NoneType' error during model loading.
RUN mkdir -p /app/tokenizer && \
    python3 -m vllm_plugin.tools.generate_tokenizer_files --output /app/tokenizer

# 4. Set environment variables
ENV VIBEVOICE_FFMPEG_MAX_CONCURRENCY="64" \
    PYTORCH_ALLOC_CONF="expandable_segments:True" \
    HF_XET_HIGH_PERFORMANCE="1" \
    TOKENIZER_PATH="/app/tokenizer"

# No need to change ENTRYPOINT; vllm-openai uses 'python3 -m vllm.entrypoints.openai.api_server'
# We will pass the arguments during 'docker run' or via CMD
