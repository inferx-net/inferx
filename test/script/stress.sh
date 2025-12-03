#!/usr/bin/env bash

for i in {1..10000}; do
    echo "**************Iteration: $i**************" 
    date

    
    # test/ixtest/target/debug/ixtest 1000 20 "Qwen/Qwen2.5-Coder-1.5B-Instruct-1" "Qwen/Qwen2.5-Coder-1.5B-Instruct"
    # test/ixtest/target/debug/ixtest 5 30 "Qwen/Qwen2.5-Math-7B-Instruct" "Qwen/Qwen2.5-Math-7B-Instruct"
    # test/ixtest/target/debug/ixtest 700 10 "Qwen/Qwen2.5-Coder-14B-Instruct-GPTQ-Int8-1" "Qwen/Qwen2.5-Coder-14B-Instruct-GPTQ-Int8"
    # test/ixtest/target/debug/ixtest 10 40 "Qwen/Qwen2.5-Math-1.5B-1" "Qwen/Qwen2.5-Math-1.5B"
    
    # test/ixtest/target/debug/ixtest 6 100 "Qwen/Qwen2.5-Math-1.5B-1" "Qwen/Qwen2.5-Math-1.5B"
    # test/ixtest/target/debug/ixtest 200 10 "Qwen/Qwen2.5-7B-Instruct-GPTQ-Int8" 
    # test/ixtest/target/debug/ixtest 700 100 "Qwen/Qwen2.5-Coder-1.5B-Instruct" "Qwen/Qwen2.5-Coder-1.5B-Instruct"
    # test/ixtest/target/debug/ixtest 5 100 "Qwen/Qwen2.5-Math-1.5B" "Qwen/Qwen2.5-Math-1.5B"
    # test/ixtest/target/debug/ixtest 200 10 "Qwen/Qwen2.5-Coder-14B-Instruct-GPTQ-Int8"
    #test/ixtest/target/debug/ixtest 700 10 "Qwen/Qwen2.5-Coder-3B"
    #test/ixtest/target/debug/ixtest 300 10 "Qwen/Qwen2.5-Coder-7B-Instruct-GPTQ-Int4"
    
    test/ixtest/target/debug/ixtest 1000 20 "Qwen/Qwen2.5-Coder-1.5B-Instruct" "Qwen/Qwen2.5-Coder-1.5B-Instruct"
    test/ixtest/target/debug/ixtest 5 30 "Qwen/Qwen2.5-Math-7B-Instruct" "Qwen/Qwen2.5-Math-7B-Instruct"
    test/ixtest/target/debug/ixtest 700 10 "Qwen/Qwen2.5-Coder-14B-Instruct-GPTQ-Int8" "Qwen/Qwen2.5-Coder-14B-Instruct-GPTQ-Int8"
    test/ixtest/target/debug/ixtest 10 40 "Qwen/Qwen2.5-Math-1.5B" "Qwen/Qwen2.5-Math-1.5B"

    # test/ixtest/target/debug/test/ixtest/target/debug/ixtest 450 10 "deepseek-ai/DeepSeek-R1-Distill-Qwen-1.5B"
done