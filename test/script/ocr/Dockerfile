# docker build -t inferx-ocr .

# sudo rm /opt/inferx/log/inferx.log -rf; sudo docker run \
#     --rm --name swap \
#     --runtime inferx \
#      --network host \
#     -v /opt/inferx/cache:/root/.cache/huggingface \
#     -v /opt/inferx:/opt/inferx \
#     -v /dev/shm:/dev/shm \
#     --shm-size=16g --ipc=host \
#     --annotation initgpus="0,1" \
#     --annotation gpus="0,1" \
#     --annotation vRam_slot_count="55" \
#     --env "LD_LIBRARY_PATH=/usr/local/lib/python3.12/dist-packages/nvidia/cuda_runtime/lib:/opt/inferx/bin/:/usr/local/lib/python3.12/dist-packages/nvidia/cuda_nvrtc/lib/:$LD_LIBRARY_PATH" \
#     --env "LD_PRELOAD=/opt/inferx/bin/libcudaproxy.so" \
#     --env "VLLM_ALLREDUCE_USE_SYMM_MEM=1" \
#     --env "HUGGING_FACE_HUB_TOKEN=${HF_TOKEN}" \
#     -p 8000:8000 \
#     inferx-ocr

# curl http://localhost:8000/v1/chat/completions   -H "Content-Type: application/json"   -d '{
#     "messages": [
#       {
#         "role": "user",
#         "content": [
#           {"type": "text", "text": "Describe this image"},
#           {
#             "type": "image_url",
#             "image_url": {
#               "url": "https://raw.githubusercontent.com/PaddlePaddle/PaddleOCR/release/2.7/doc/imgs_en/img_12.jpg"
#             }
#           }
#         ]
#       }
#     ],
#     "max_tokens": 20
#   }'


# ---- Base: official vLLM image ----
FROM vllm/vllm-openai:glm5
WORKDIR /app

# System deps for OpenCV
RUN apt-get update && apt-get install -y \
    libgl1 \
    libglib2.0-0 \
    && rm -rf /var/lib/apt/lists/*

RUN pip install --no-cache-dir \
    pillow==11.1.0 \
    numpy==1.26.4 \
    opencv-python-headless==4.10.0.84 \
    pypdfium2==5.3.0 \
    fastapi \
    uvicorn \
    requests \
    huggingface_hub[hf_transfer]

ENV HF_HUB_ENABLE_HF_TRANSFER=1
ENV HF_XET_HIGH_PERFORMANCE=1

COPY server.py .
RUN chmod +x server.py

EXPOSE 8000

# This line is crucial to prevent the "vllm: error"
ENTRYPOINT []

# Now this will run as a standard python script
CMD ["python3", "server.py"]