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