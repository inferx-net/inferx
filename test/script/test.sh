nohup ./stopstatesvc.sh > stopstatesvc.log 2>&1 &

nohup test/script/stress.sh > stress.log 2>&1 &
nohup test/script/stopscheduler.sh > stopscheduler.log 2>&1 &

nohup test/script/stopstatesvc.sh > stopstatesvc.log 2>&1 &

curl -s http://localhost:31502/debug/state | jq . >sch.json

curl -s http://localhost:31501/debug/func_agents | jq . >func_agents.json



ps aux | grep stop

ps aux | grep stress