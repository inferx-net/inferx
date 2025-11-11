run_hey() {
    local concurrency=$1
    local duration=$2
    local model=$3
    local token=$4
    local model2=$5
    local url="http://localhost:31501/funccall/public/${model}/v1/completions"

    echo "=== Running hey for model: ${model} (c=${concurrency}, z=${duration}) ==="

    hey -c "${concurrency}" -z "${duration}" -m POST \
        -H "Content-Type: application/json" \
        -d "{
            \"prompt\": \"Can you provide ways to eat combinations of bananas and dragonfruits?\",
            \"max_tokens\": \"${token}\",
            \"model\": \"${model2}\",
            \"stream\": \"true\",
            \"temperature\": \"0\"
        }" \
        "${url}" | awk '/Status code distribution:/{flag=1; next} flag'
}


run_hey 20 36000s "Qwen/Qwen2.5-Coder-1.5B-Instruct" 800 "Qwen/Qwen2.5-Coder-1.5B-Instruct"


# for i in {1..50}; do
#     echo "Iteration: $i"

#     /opt/inferx/bin/ixctl update default_funcpolicy.json
#    run_curl "Qwen/Qwen2.5-1.5B"
#     # sleep 10
#     run_curl "Qwen/Qwen2.5-Coder-1.5B-Instruct"

#      # sleep 10
#     # run_hey 400 20s "Qwen/Qwen2.5-Coder-3B"
#     # # sleep 10
#     # run_hey 200 20s "Qwen/Qwen2.5-Coder-7B-Instruct"
#     sleep 10
# done

