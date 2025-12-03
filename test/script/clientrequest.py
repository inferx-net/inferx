import http.client
import json


def main():
    host = "localhost"
    port = 31501
    path = "/funccall/public/Qwen/Qwen2.5-Coder-14B-Instruct-GPTQ-Int8/v1/completions"

    payload = json.dumps(
        {
            "max_tokens": "800",
            "temperature": "0",
            "model": "Qwen/Qwen2.5-Coder-14B-Instruct-GPTQ-Int8",
            "prompt": "write a quick sort algorithm.",
            "stream": "true",
        }
    )

    headers = {
        "Host": f"{host}:{port}",
        "Content-Type": "application/json",
        "Content-Length": str(len(payload)),
    }

    conn = http.client.HTTPConnection(host, port, timeout=120)
    conn.request("POST", path, body=payload, headers=headers)
    resp = conn.getresponse()
    body = resp.read()

    print(f"Status: {resp.status} {resp.reason}")
    print("Headers:")
    for k, v in resp.getheaders():
        print(f"  {k}: {v}")

    print("Body:")
    print(body.decode("utf-8", errors="replace"))

    conn.close()


if __name__ == "__main__":
    main()
