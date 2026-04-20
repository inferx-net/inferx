nohup ./stopstatesvc.sh > stopstatesvc.log 2>&1 &

nohup test/script/stress.sh > stress.log 2>&1 &
nohup test/script/stopscheduler.sh > stopscheduler.log 2>&1 &

nohup test/script/stopnodeagent.sh > stopnodeagent.log 2>&1 &

nohup test/script/stopstatesvc.sh > stopstatesvc.log 2>&1 &

curl -s http://localhost:31502/debug/state | jq . >sch.json

curl -s http://localhost:31501/debug/func_agents | jq . >func_agents.json

nohup test/script/stopscheduler-cw.sh > stopscheduler-cw.log 2>&1 &

nohup test/script/stopixproxy-cw.sh > stopixproxy-cw.log 2>&1 &


test/ixtest/target/debug/ixtest concurrency=10 duration=20 alias="Qwen/Llama-3.2-3B-Instruct-FP8" model="neuralmagic/Llama-3.2-3B-Instruct-FP8" endpoint="http://166.19.16.217:4000" prompt="write a quick sor
t algorithm." max_tokens=100 show_output=true

test/ixtest/target/debug/ixtest concurrency=40 duration=37 alias="Qwen/Qwen3-235B-A22B-Thinking-2507" model="Qwen/Qwen3-235B-A22B-Thinking-2507" endpoint="http://166.19.16.217:4000" prompt="Can you provide ways to eat combinations of bananas and dragonfruits?" max_tokens=100 show_output=true



ps aux | grep stop

ps aux | grep stress


curl -X POST http://localhost:31501/funccall/public/Qwen/gemma-4-E4B-it/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"max_tokens": "800", "model": "google/gemma-4-E4B-it", "temperature": "0", "messages": [{"role": "user", "content": [{"type": "text", "text": "Describe this image"}, {"type": "image_url", "image_url": {"url": "http://images.cocodataset.org/val2017/000000039769.jpg"}}]}], "stream": false}'

https://www.kozco.com/tech/piano2.wav


curl -X POST http://localhost:31501/funccall/public/Qwen/gemma-4-E4B-it/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"max_tokens": "800", "model": "google/gemma-4-E4B-it", "temperature": "0", "messages": [{"role": "user", "content": [{"type": "text", "text": "Describe this image"}, {"type": "image_url", "image_url": {"url": "http://images.cocodataset.org/val2017/000000039769.jpg"}}]}], "stream": false}'


curl -X POST http://localhost:31501/funccall/public/Qwen/gemma-4-E4B-it/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
    "max_tokens": "800",
    "model": "google/gemma-4-E4B-it",
    "temperature": "0",
    "messages": [
      {
        "role": "user",
        "content": [
          { "type": "text", "text": "Describe the sounds you hear in this audio" },
          {
            "type": "audio_url",
            "audio_url": {
              "url": "https://www.kozco.com/tech/piano2.wav"
            }
          }
        ]
      }
    ],
    "stream": false
  }'

curl -X POST http://localhost:31501/funccall/public/Qwen/gemma-4-E4B-it/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"max_tokens": "800", "model": "google/gemma-4-E4B-it", "temperature": "0", "messages": [{"role": "user", "content": [{"type": "text", "text": "Describe this image"}, {"type": "image_url", "image_url": {"url": "http://images.cocodataset.org/val2017/000000039769.jpg"}}]}], "stream": false}'


curl -X POST \
  http://localhost:31501/funccall/public/Qwen/gemma-4-E4B-it/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
    "max_tokens": "800",
    "model": "google/gemma-4-E4B-it",
    "temperature": "0",
    "messages": [
      {
        "role": "user",
        "content": [
          {
            "type": "text",
            "text": "Describe the sounds you hear in this audio"
          },
          {
            "type": "audio_url",
            "audio_url": {
              "url": "https://www.kozco.com/tech/piano2.wav"
            }
          }
        ]
      }
    ],
    "stream": false
  }'

curl -X POST \
  http://localhost:31501/funccall/public/Qwen/gemma-4-E4B-it-ix/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
    "max_tokens": "800",
    "model": "google/gemma-4-E4B-it",
    "temperature": "0",
    "messages": [
      {
        "role": "user",
        "content": [
          {
            "type": "text",
            "text": "Write a short summary of how rain forms."
          }
        ]
      }
    ],
    "stream": false
  }'



curl -X POST \
  http://localhost:31501/funccall/public/Qwen/gemma-4-E4B-it-ix/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
    "max_tokens": "800",
    "model": "google/gemma-4-E4B-it",
    "temperature": "0",
    "messages": [
      {
        "role": "user",
        "content": [
          {
            "type": "text",
            "text": "Describe the sounds you hear in this audio"
          },
          {
            "type": "audio_url",
            "audio_url": {
              "url": "https://www.kozco.com/tech/piano2.wav"
            }
          }
        ]
      }
    ],
    "stream": false
  }'


curl -X POST http://localhost:31501/funccall/public/Qwen/gemma-4-E4B-it-ix/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
    "max_tokens": "800",
    "model": "google/gemma-4-E4B-it",
    "temperature": "0",
    "messages": [
      {
        "role": "user",
        "content": [
          { "type": "text", "text": "Describe the sounds you hear in this audio" },
          {
            "type": "audio_url",
            "audio_url": {
              "url": "https://www.kozco.com/tech/piano2.wav"
            }
          }
        ]
      }
    ],
    "stream": false
  }'

curl http://localhost:31501/funccall/public/default/Qwen2.5-Coder-0.5B/v1/models

curl http://localhost:31501/funccall/public/default/phi4_history/v1/models

curl -X POST http://localhost:31501/funccall/public/Trial/phi4_african_history_lora_ds2_axolotl/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"max_tokens": "10", "messages": [{"content": "write a quick sort algorithm.", "role": "user"}], "model": "phi4_african_history", "stream": "true", "temperature": "0"}'


curl -X POST http://localhost:31501/funccall/tn-pvgiouccgp/default/Phi-4-mini-instruct/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -H 'Authorization: Bearer <INFERENCE_API_KEY(Find or create one on Admin|Apikeys page)>' \
  -d '{"max_tokens": "1000", "messages": [{"content": "write a quick sort algorithm.", "role": "user"}], "model": "phi4_african_history", "stream": "true", "temperature": "0"}'


curl -X POST  http://localhost:31501/funccall/public/Qwen/whisper-tiny/v1/audio/transcriptions \
    -H "Content-Type: multipart/form-data" \
    -F "file=@/home/qq/code/inferx/config/clone_test.wav" \
    -F "response_format=json" \
    -F "language=en"


curl http://localhost:31501/funccall/public/Qwen/whisper-tiny/v1/models


curl -X POST http://localhost:31501/funccall/public/Qwen/inferx-buddle1/m2/rerank \
  -H "Content-Type: application/json" \
  -d '{
    "model": "Qwen/Qwen3-Reranker-0.6B",
    "query": "What is the capital of China?",
    "documents": [
        "The capital of China is Beijing.",
        "Gravity is a force that attracts two bodies towards each other."
    ],
    "top_n": 2
  }'

curl -X POST http://localhost:31501/funccall/public/Qwen/inferx-buddle1/m3/rerank \
  -H "Content-Type: application/json" \
  -d '{
    "model": "Qwen/Qwen3-Reranker-0.6B",
    "query": "What is the capital of China?",
    "documents": [
        "The capital of China is Beijing.",
        "Gravity is a force that attracts two bodies towards each other."
    ],
    "top_n": 2
  }'

curl -X POST http://localhost:31501/funccall/public/Qwen/inferx-buddle1/m1/v1/completions \
  -H 'Content-Type: application/json' \
  -d '{"max_tokens": "10", "model": "Qwen/Qwen2.5-1.5B", "stream": "true", "temperature": "0", "prompt": "Can you provide ways to eat combinations of bananas and dragonfruits?"}'

curl -X POST http://localhost:31501/funccall/public/Qwen/inferx-buddle1/v1/completions \
  -H 'Content-Type: application/json' \
  -d '{"max_tokens": "10", "model": "Qwen/Qwen2.5-1.5B", "stream": "true", "temperature": "0", "prompt": "Can you provide ways to eat combinations of bananas and dragonfruits?"}'


curl http://localhost:31501/funccall/public/Qwen/inferx-buddle1/v1/models






curl -X POST  https://model.inferx.net/funccall/public/Trial/whisper-tiny/v1/audio/transcriptions     -H "Content-Type: multipart/form-data"     -F "file=@/home/qq/code/inferx/config/clone_test.wav"     -F "response_format=json"     -F "language=en" | jq .


curl -X POST  https://dev1.inferx.net/funccall/public/Qwen/whisper-tiny/v1/audio/transcriptions     -H "Content-Type: multipart/form-data"     -F "file=@/home/qq/code/inferx/config/clone_test.wav"     -F "response_format=json"     -F "language=en" | jq .