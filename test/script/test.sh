nohup ./stopstatesvc.sh > stopstatesvc.log 2>&1 &

nohup test/script/stress.sh > stress.log 2>&1 &
nohup test/script/stopscheduler.sh > stopscheduler.log 2>&1 &

ps aux | grep stop

ps aux | grep stress