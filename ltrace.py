import re

# Example input log lines
log_file = "./a.txt"  # or change this to your actual file path

# Regular expression to match cudnn function calls
cudnn_pattern = re.compile(r'->(cudnn\w+)\(')

# Set for deduplication
api_set = set()

with open(log_file, 'r') as f:
    for line in f:
        match = cudnn_pattern.search(line)
        if match:
            api_set.add(match.group(1))

# Sort and print as list
api_list = sorted(api_set)
for api in api_list:
    print(api)
