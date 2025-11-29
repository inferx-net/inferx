nohup ./stopstatesvc.sh > stopstatesvc.log 2>&1 &

nohup ./stress.sh > stress.log 2>&1 &

ps aux | grep stop

ps aux | grep stress