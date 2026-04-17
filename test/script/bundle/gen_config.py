import sys

def generate_chained_config(commands):
    config_template = [
        "[supervisord]",
        "nodaemon=true",
        "user=root",
        "",
        "[program:nginx]",
        "command=nginx -g 'daemon off;'",
        "autorestart=true",
        "stdout_logfile=/dev/stdout",
        "stdout_logfile_maxbytes=0",
        "stderr_logfile=/dev/stderr",
        "stderr_logfile_maxbytes=0",
        ""
    ]
    # Map to store ports to wait for
    # We assume the user passes commands with --port XXXX
    ports = []
    for cmd in commands:
        # Simple extraction of port number from the command string
        try:
            port = cmd.split("--port ")[1].split(" ")[0]
            ports.append(port)
        except IndexError:
            ports.append(None)
    for i, cmd in enumerate(commands):
        # The first process (i=0) starts immediately.
        # Subsequent processes wait for the port of the previous process.
        if i > 0 and ports[i-1]:
            wait_port = ports[i-1]
            wait_cmd = f"until curl -s http://localhost:{wait_port}/health; do echo 'Waiting for vllm{i} (port {wait_port}) health...'; sleep 10; done; "
            final_cmd = f"/bin/sh -c \"{wait_cmd}{cmd}\""
        else:
            final_cmd = cmd
        program_section = [
            f"[program:vllm{i+1}]",
            f"command={final_cmd}",
            "autorestart=false",
            "stdout_logfile=/dev/stdout",
            "stdout_logfile_maxbytes=0",
            "stderr_logfile=/dev/stderr",
            "stderr_logfile_maxbytes=0",
            ""
        ]
        config_template.extend(program_section)

    with open("supervisord.conf", "w") as f:
        f.write("\n".join(config_template))
    print(f"Generated chained config for {len(commands)} instances.")

if __name__ == "__main__":
    if len(sys.argv) < 2:
        print("Usage: python3 gen_config.py 'vllm_cmd1' 'vllm_cmd2' ...")
        sys.exit(1)
    generate_chained_config(sys.argv[1:])
    print("finish gen conf...")
