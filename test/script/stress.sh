#!/usr/bin/env bash

for i in {1..10000}; do
    echo "**************Iteration: $i**************" 
    date

    /inferx/ixtest 6 100 "Qwen/Qwen2.5-Math-1.5B-1" "Qwen/Qwen2.5-Math-1.5B"
    # /inferx/test/ixtest/target/debug/ixtest 200 10 "Qwen/Qwen2.5-7B-Instruct-GPTQ-Int8" 
    /inferx/ixtest 700 100 "Qwen/Qwen2.5-Coder-1.5B-Instruct" "Qwen/Qwen2.5-Coder-1.5B-Instruct"
    /inferx/ixtest 5 100 "Qwen/Qwen2.5-Math-1.5B" "Qwen/Qwen2.5-Math-1.5B"
    # /inferx/test/ixtest/target/debug/ixtest 200 10 "Qwen/Qwen2.5-Coder-14B-Instruct-GPTQ-Int8"
    # /inferx/test/ixtest/target/debug/ixtest 700 10 "Qwen/Qwen2.5-Coder-3B"
    # /inferx/test/ixtest/target/debug/ixtest 300 10 "Qwen/Qwen2.5-Coder-7B-Instruct-GPTQ-Int4"
    # /inferx/test/ixtest/target/debug/ixtest 200 10 "Qwen/Qwen2.5-Coder-7B-Instruct"
    # /inferx/test/ixtest/target/debug/ixtest 450 10 "deepseek-ai/DeepSeek-R1-Distill-Qwen-1.5B"
done