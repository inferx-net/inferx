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
from fastapi import FastAPI, Request, HTTPException, status
from fastapi.responses import JSONResponse

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
        "--gpu-memory-utilization", "0.80", 
        "--max-model-len", "8192",
        "--limit-mm-per-prompt", '{"image": 1, "image_tokens": 8192}',
    ]
    
    print(f"--- Starting vLLM Engine (Subprocess) ---")
    vllm_proc = subprocess.Popen(vllm_cmd)

    # 2. Wait for vLLM Readiness URL
    print(f"Waiting for vLLM Health API at http://localhost:{VLLM_PORT}/health ...")
    ready = False
    
    for i in range(300): 
        # Create a fresh client for EVERY attempt to ensure no connection pooling overlap
        async with httpx.AsyncClient() as client:
            try:
                # Use a reasonable timeout; 5.0s is usually enough for a local health check
                response = await client.get(f"http://localhost:{VLLM_PORT}/health", timeout=5.0)
                if response.status_code == 200:
                    ready = True
                    break
            except (httpx.HTTPError, httpx.StreamError):
                # This catches ConnectError, ReadError, Timeout, etc. 
                # We catch the base classes to be as broad as possible during boot.
                pass
        
        # Check if the process crashed while we were waiting
        if vllm_proc.poll() is not None:
            print("--- vLLM Process Crash Detected ---")
            raise RuntimeError("vLLM process died during startup. Check GPU/NCCL logs.")
        
        if i % 10 == 0:
            print(f"Bootstrapping vLLM ({i}s)... Probe failed (expected), retrying...")
        
        # Wait 1 second BEFORE opening the next connection
        await asyncio.sleep(10)
    
    if not ready:
        vllm_proc.terminate()
        raise TimeoutError("vLLM Health API did not become ready in time.")

    print("vLLM is healthy and ready for inference.")

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

    # Cleanup on shutdown
    print("Shutting down vLLM...")
    vllm_proc.terminate()
    try:
        vllm_proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        vllm_proc.kill()
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

def is_system_ready():
    """Checks if all internal components are loaded and healthy."""
    return (
        hasattr(app.state, "layout_model") and 
        app.state.layout_model is not None and
        hasattr(app.state, "layout_processor")
    )

@app.get("/health")
@app.get("/v1/health")
async def health_check():
    """Custom health check for InferX monitoring."""
    if is_system_ready():
        return {"status": "healthy", "vllm_port": VLLM_PORT}
    return JSONResponse(
        status_code=status.HTTP_503_SERVICE_UNAVAILABLE,
        content={"status": "initializing", "message": "Models are still loading into VRAM."}
    )

@app.get("/v1/models")
async def list_models():
    """OpenAI-compatible models endpoint."""
    if not is_system_ready():
        raise HTTPException(status_code=503, detail="System initializing")
    
    return {
        "object": "list",
        "data": [
            {
                "id": OCR_MODEL_ID,
                "object": "model",
                "created": 1700000000,
                "owned_by": "inferx"
            }
        ]
    }

@app.post("/v1/chat/completions")
async def chat_proxy(request: Request):
    body = await request.json()
    messages = body.get("messages", [])
    
    # We only support a single user message for this custom crop-and-merge logic
    # Find the image and the prompt
    img = None
    original_prompt = "Perform OCR on this region."
    
    for msg in messages:
        if msg["role"] == "user":
            for item in msg["content"]:
                if item["type"] == "text":
                    original_prompt = item["text"]
                if item["type"] == "image_url":
                    url = item["image_url"]["url"]
                    if url.startswith("data:image"):
                        _, encoded = url.split(",", 1)
                        img = Image.open(BytesIO(base64.b64decode(encoded))).convert("RGB")
                    else:
                        async with httpx.AsyncClient() as client:
                            resp = await client.get(url)
                            img = Image.open(BytesIO(resp.content)).convert("RGB")

    if not img:
        raise HTTPException(status_code=400, detail="No image found in request.")

    print("start layout process:")
    # 1. Get Layout Bounding Boxes
    inputs = app.state.layout_processor(images=img, return_tensors="pt").to("cuda")
    print("end layout process:")
    with torch.no_grad():
        outputs = app.state.layout_model(**inputs)
    print("end layout process:", outputs)
    
    results = app.state.layout_processor.post_process_object_detection(
        outputs, threshold=0.35, target_sizes=[img.size[::-1]]
    )[0]
    print("end layout process:", results)
    
    # Sort boxes by Y-coordinate (top-to-bottom reading order)
    sorted_indices = torch.argsort(results["boxes"][:, 1])
    
    merged_results = []
    
    async with httpx.AsyncClient() as client:
        # 2. Iterate through regions and crop
        for idx in sorted_indices:
            box = results["boxes"][idx].tolist()
            label = app.state.layout_model.config.id2label[results["labels"][idx].item()]
            
            # Crop image: [xmin, ymin, xmax, ymax]
            cropped_img = img.crop((box[0], box[1], box[2], box[3]))
            
            # Convert crop to Base64
            buffered = BytesIO()
            cropped_img.save(buffered, format="JPEG")
            img_b64 = base64.b64encode(buffered.getvalue()).decode()
            
            print("start labels:")
            print(label)
            print("end labels:")
            # 3. Send Region to GLM-OCR
            vllm_payload = {
                "model": body.get("model", OCR_MODEL_ID),
                "messages": [{
                    "role": "user",
                    "content": [
                        {"type": "text", "text": f"Label: {label}. {original_prompt}"},
                        {"type": "image_url", "image_url": {"url": f"data:image/jpeg;base64,{img_b64}"}}
                    ]
                }],
                "max_tokens": 512,
                "temperature": 0
            }
            
            try:
                vllm_resp = await client.post(
                    f"http://localhost:{VLLM_PORT}/v1/chat/completions",
                    json=vllm_payload,
                    timeout=60.0
                )
                chunk_text = vllm_resp.json()["choices"][0]["message"]["content"]
                merged_results.append(f"### {label}\n{chunk_text}")
            except Exception as e:
                print(f"Error processing region {idx}: {e}")

    # 4. Merge results and return as a standard OpenAI response
    final_content = "\n\n".join(merged_results)
    
    return {
        "id": "inferx-merged",
        "object": "chat.completion",
        "created": int(time.time()),
        "model": OCR_MODEL_ID,
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": final_content
            },
            "finish_reason": "stop"
        }]
    }


if __name__ == "__main__":
    uvicorn.run("server:app", host="0.0.0.0", port=SERVER_PORT, reload=False)