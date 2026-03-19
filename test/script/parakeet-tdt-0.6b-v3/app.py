import os
import tempfile
import torch
import shutil
import uvicorn
import httpx
import torchaudio
import soundfile as sf
import nemo.collections.asr as nemo_asr
from fastapi import FastAPI, UploadFile, File, HTTPException
from pydantic import BaseModel
from typing import List, Optional, Union

app = FastAPI(title="Parakeet ASR & Chat Server")

MODEL_NAME = "nvidia/parakeet-tdt-0.6b-v3"

print("Loading ASR model...")
asr_model = nemo_asr.models.ASRModel.from_pretrained(model_name=MODEL_NAME)
asr_model.preprocessor.to("cpu")
device = "cuda" if torch.cuda.is_available() else "cpu"
asr_model = asr_model.to(device)
print(f"Model loaded on {device}")

# --- Pydantic Models for OpenAI-like Schema ---

class AudioUrl(BaseModel):
    url: str

class ContentItem(BaseModel):
    type: str
    text: Optional[str] = None
    audio_url: Optional[AudioUrl] = None

class Message(BaseModel):
    role: str
    content: Union[str, List[ContentItem]]

class ChatCompletionRequest(BaseModel):
    model: Optional[str] = "nvidia/parakeet-tdt-0.6b-v3"
    messages: List[Message]
    stream: Optional[bool] = False

# --- Helper Functions ---

async def download_audio(url: str) -> str:
    """Downloads audio from a URL to a temporary file."""
    suffix = os.path.splitext(url.split("?")[0])[1] or ".wav"
    tmp = tempfile.NamedTemporaryFile(delete=False, suffix=suffix)
    async with httpx.AsyncClient() as client:
        try:
            response = await client.get(url, follow_redirects=True)
            response.raise_for_status()
            with open(tmp.name, "wb") as f:
                f.write(response.content)
            return tmp.name
        except Exception as e:
            os.remove(tmp.name)
            raise HTTPException(status_code=400, detail=f"Failed to download audio: {e}")

def process_audio_file(path: str):
    audio, sr = sf.read(path)
    if audio.ndim > 1:
        audio = audio.mean(axis=1)
    
    audio_tensor = torch.tensor(audio, dtype=torch.float32)
    
    if sr != 16000:
        audio_tensor = torchaudio.functional.resample(
            audio_tensor.unsqueeze(0), orig_freq=sr, new_freq=16000
        ).squeeze(0)
    
    results = asr_model.transcribe(
        audio=[audio_tensor], 
        batch_size=1, 
        return_hypotheses=True
    )
    
    hyp = results[0]
    duration = len(audio_tensor) / 16000.0

    # From your DEBUG log: attribute is 'timestamp', not 'timestep'
    # It is a tensor: tensor([0, 1, 2, 4, ...], device='cuda:0')
    raw_timestamps = getattr(hyp, 'timestamp', None)

    if raw_timestamps is None:
        return {"text": hyp.text, "timestamps": [], "error": "No timestamp attribute found"}

    # Convert CUDA tensor to a Python list
    if torch.is_tensor(raw_timestamps):
        timesteps_list = raw_timestamps.cpu().numpy().tolist()
    else:
        timesteps_list = raw_timestamps

    # Calculation for TDT 0.6b:
    # Your log shows a 'length' of 125. This is the total frame count.
    total_frames = hyp.length.item() if hasattr(hyp, 'length') and torch.is_tensor(hyp.length) else 125
    
    time_per_frame = duration / total_frames if total_frames > 0 else 0.08
    
    # Map the frame indices to seconds
    ts_seconds = [round(t * time_per_frame, 2) for t in timesteps_list]

    return {
        "text": hyp.text,
        "timestamps": ts_seconds,
        "duration": round(duration, 2)
    }

@app.get("/health")
async def health_check():
    return {
        "status": "healthy",
        "device": device,
        "model": MODEL_NAME
    }

@app.post("/v1/audio/transcriptions")
async def transcribe(file: UploadFile = File(...)):
    suffix = os.path.splitext(file.filename)[1]
    with tempfile.NamedTemporaryFile(delete=False, suffix=suffix) as tmp:
        shutil.copyfileobj(file.file, tmp)
        path = tmp.name
    try:
        result = process_audio_file(path)
        return result  # Return the dict directly
    finally:
        if os.path.exists(path):
            os.remove(path)

@app.post("/v1/chat/completions")
async def chat_completions(request: ChatCompletionRequest):
    """
    Simulates a chat completion by transcribing any audio_url found in the 
    last message and returning it as a response.
    """
    last_msg = request.messages[-1]
    transcript = ""

    if isinstance(last_msg.content, list):
        for item in last_msg.content:
            if item.type == "audio_url" and item.audio_url:
                audio_path = await download_audio(item.audio_url.url)
                try:
                    transcript = process_audio_file(audio_path)
                finally:
                    os.remove(audio_path)
    
    # Return in OpenAI format
    return {
        "id": "chatcmpl-parakeet",
        "object": "chat.completion",
        "created": 123456789,
        "model": request.model,
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": transcript if transcript else "No audio detected to transcribe."
            },
            "finish_reason": "stop"
        }]
    }

if __name__ == "__main__":
    uvicorn.run(app, host="0.0.0.0", port=8000)