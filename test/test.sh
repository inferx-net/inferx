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


run_hey() {
    local concurrency=$1
    local duration=$2
    local model=$3
    local url="http://localhost:4000/funccall/public/${model}/v1/completions"

    echo "=== Running hey for model: ${model} (c=${concurrency}, z=${duration}) ==="

    hey -c "${concurrency}" -z "${duration}" -m POST \
        -H "Content-Type: application/json" \
        -d "{
            \"prompt\": \"Can you provide ways to eat combinations of bananas and dragonfruits?\",
            \"max_tokens\": \"10\",
            \"model\": \"${model}\",
            \"stream\": \"true\",
            \"temperature\": \"0\"
        }" \
        "${url}" | awk '/Status code distribution:/{flag=1; next} flag'
}

run_curl() {
    local model=$1
    local url="http://localhost:4000/funccall/public/${model}/v1/completions"

    echo "=== Running curl for model: ${model} ==="

    curl -s -X POST "${url}" \
        -H "Content-Type: application/json" \
        -d "{
            \"prompt\": \"Can you provide ways to eat combinations of bananas and dragonfruits?\",
            \"max_tokens\": 20,
            \"model\": \"${model}\",
            \"stream\": false,
            \"temperature\": 0
        }" 
}

for i in {1..50}; do
    echo "Iteration: $i"

    /home/brad/rust/inferx/test/ixtest/target/debug/ixtest 200 20 "Qwen/Qwen2.5-1.5B"
    # run_curl "Qwen/Qwen2.5-1.5B"
    # sleep 10
    /home/brad/rust/inferx/test/ixtest/target/debug/ixtest 200 20 "Qwen/Qwen2.5-Coder-1.5B-Instruct"
    # run_curl "Qwen/Qwen2.5-Coder-1.5B-Instruct"
    # sleep 10
    /home/brad/rust/inferx/test/ixtest/target/debug/ixtest 200 20 "Qwen/Qwen2.5-Coder-3B"
    # # sleep 10
    # run_hey 200 20s "Qwen/Qwen2.5-Coder-7B-Instruct"
    # # sleep 10
done
