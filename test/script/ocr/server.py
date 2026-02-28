import os
import sys
import time
import socket
import asyncio
import subprocess
import base64
import torch
import httpx
import uvicorn
from io import BytesIO
from contextlib import asynccontextmanager
from fastapi import FastAPI, Request, HTTPException
from PIL import Image

# =============================================================================
# Configuration
# =============================================================================
OCR_MODEL_ID = os.getenv("OCR_MODEL_ID", "zai-org/GLM-OCR")
LAYOUT_MODEL_ID = os.getenv("LAYOUT_MODEL_ID", "PaddlePaddle/PP-DocLayoutV3_safetensors")
VLLM_PORT = int(os.getenv("VLLM_PORT", 8001))
SERVER_PORT = int(os.getenv("SERVER_PORT", 8000))

# =============================================================================
# Lifespan Management
# =============================================================================
@asynccontextmanager
async def lifespan(app: FastAPI):
    # 1. Start vLLM Subprocess
    vllm_cmd = [
        sys.executable, "-m", "vllm.entrypoints.openai.api_server",
        "--model", OCR_MODEL_ID,
        "--port", str(VLLM_PORT),
        "--trust-remote-code",
        "--dtype", "bfloat16",
        "--gpu-memory-utilization", "0.80", # Reserve VRAM for Layout Model
        "--max-model-len", "8192",
        "--limit-mm-per-prompt", '{"image": 1, "image_tokens": 8192}',
    ]
    
    print(f"--- Starting vLLM Engine (Subprocess) ---")
    vllm_proc = subprocess.Popen(vllm_cmd)

    # 2. Wait for vLLM Ready
    ready = False
    for _ in range(300):
        try:
            with socket.create_connection(("localhost", VLLM_PORT), timeout=1):
                ready = True
                break
        except (OSError, ConnectionRefusedError):
            if vllm_proc.poll() is not None:
                raise RuntimeError("vLLM failed to start.")
            time.sleep(1)
    
    if not ready:
        vllm_proc.terminate()
        raise TimeoutError("vLLM startup timed out.")

    # 3. Load Layout Model into Main Process
    from transformers import (
        PPDocLayoutV3ForObjectDetection,
        PPDocLayoutV3ImageProcessorFast,
    )

    print(f"--- Loading Layout Model: {LAYOUT_MODEL_ID} ---")
    app.state.layout_processor = PPDocLayoutV3ImageProcessorFast.from_pretrained(LAYOUT_MODEL_ID)
    app.state.layout_model = PPDocLayoutV3ForObjectDetection.from_pretrained(LAYOUT_MODEL_ID).to("cuda")
    app.state.layout_model.eval()
    
    print("Full Pipeline Ready.")

    yield # Running...

    vllm_proc.terminate()
    vllm_proc.wait(timeout=10)

# =============================================================================
# Helper: Layout to Text Context
# =============================================================================
def get_layout_context(image: Image.Image, processor, model):
    """Runs layout detection and returns a string describing the structure."""
    inputs = processor(images=image, return_tensors="pt").to("cuda")
    with torch.no_grad():
        outputs = model(**inputs)

    # Convert raw outputs to bounding boxes
    results = processor.post_process_object_detection(
        outputs, 
        threshold=0.35, 
        target_sizes=[image.size[::-1]]
    )[0]

    layout_items = []
    for score, label_id, box in zip(results["scores"], results["labels"], results["boxes"]):
        label = model.config.id2label[label_id.item()]
        coords = [int(c) for c in box.tolist()] # [xmin, ymin, xmax, ymax]
        layout_items.append(f"Detected {label} at region {coords}")

    return "\n".join(layout_items) if layout_items else ""

# =============================================================================
# API Endpoints
# =============================================================================
app = FastAPI(lifespan=lifespan)

@app.post("/v1/chat/completions")
async def chat_proxy(request: Request):
    body = await request.json()
    messages = body.get("messages", [])

    # Process images found in the request
    for msg in messages:
        content = msg.get("content")
        if not isinstance(content, list):
            continue
            
        for item in content:
            if item.get("type") == "image_url":
                url = item["image_url"]["url"]
                if url.startswith("data:image"):
                    try:
                        # 1. Extract Image
                        _, encoded = url.split(",", 1)
                        img = Image.open(BytesIO(base64.b64decode(encoded))).convert("RGB")
                        
                        # 2. Run Layout Analysis
                        layout_str = get_layout_context(
                            img, 
                            app.state.layout_processor, 
                            app.state.layout_model
                        )

                        # 3. Inject Layout context into the TEXT part of the prompt
                        if layout_str:
                            for text_item in content:
                                if text_item.get("type") == "text":
                                    original_text = text_item["text"]
                                    text_item["text"] = (
                                        f"Document Structure:\n{layout_str}\n\n"
                                        f"Task: {original_text}"
                                    )
                    except Exception as e:
                        print(f"Pipeline processing error: {e}")

    # Forward modified request to vLLM
    async with httpx.AsyncClient() as client:
        try:
            vllm_resp = await client.post(
                f"http://localhost:{VLLM_PORT}/v1/chat/completions",
                json=body,
                timeout=120.0
            )
            return vllm_resp.json()
        except Exception as e:
            raise HTTPException(status_code=500, detail=f"vLLM Error: {e}")

if __name__ == "__main__":
    uvicorn.run("server:app", host="0.0.0.0", port=SERVER_PORT, reload=False)