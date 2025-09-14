ab -n 20 -v 4 -c 20 -p data.json \
    -T application/json   \
    http://localhost:31501/funccall/public/Qwen/Qwen2.5-Coder-1.5B-Instruct/v1/completions


for i in {1..100}; do
    curl http://localhost:31501/funccall/public/Qwen/Qwen2.5-Coder-1.5B-Instruct/v1/completions \
        -H "Content-Type: application/json" -d '{
            "max_tokens": "100", 
            "model": "Qwen/Qwen2.5-Coder-1.5B-Instruct", 
            "temperature": "0", 
            "stream": "false", 
            "prompt": "Can you provide ways to eat combinations of bananas and dragonfruits?" 
        }'
done