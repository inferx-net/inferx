# Copyright (c) 2021 Quark Container Authors
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http:#www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

import json
import os
import time

import requests
import markdown

from flask import (
    Blueprint,
    Flask,
    jsonify,
    render_template,
    render_template_string,
    request,
    Response,
    send_from_directory
)

from threading import Thread

app = Flask(__name__)


tls = True

apihostaddr = "http://localhost:4000"
# apihostaddr = "https://quarksoft.io:4000"

def read_markdown_file(filename):
    """Read and convert Markdown file to HTML"""
    with open(filename, "r", encoding="utf-8") as f:
        content = f.read()
    return markdown.markdown(content)


def ReadFuncLog(namespace: str, funcId: str) -> str:
    req = qobjs_pb2.ReadFuncLogReq(
        namespace=namespace,
        funcName=funcId,
    )

    channel = grpc.insecure_channel("127.0.0.1:1237")
    stub = qobjs_pb2_grpc.QMetaServiceStub(channel)
    res = stub.ReadFuncLog(req)
    return res.content


def listfuncs(tenant: str, namespace: str):
    url = "{}/functions/{}/{}/".format(apihostaddr, tenant, namespace)
    resp = requests.get(url)
    funcs = json.loads(resp.content)  

    return funcs


def getfunc(tenant: str, namespace: str, funcname: str):
    url = "{}/function/{}/{}/{}/".format(apihostaddr, tenant, namespace, funcname)
    resp = requests.get(url)
    func = json.loads(resp.content)
    return func


def listsnapshots(tenant: str, namespace: str):
    url = "{}/snapshots/{}/{}/".format(apihostaddr, tenant, namespace)
    resp = requests.get(url)
    func = json.loads(resp.content)
    return func


def listnodes():
    url = "{}/nodes/".format(apihostaddr)
    resp = requests.get(url)
    nodes = json.loads(resp.content)

    return nodes


def getnode(name: str):
    url = "{}/node/{}/".format(apihostaddr, name)
    resp = requests.get(url)
    func = json.loads(resp.content)

    return func


def listpods(tenant: str, namespace: str, funcname: str):
    url = "{}/pods/{}/{}/{}/".format(apihostaddr, tenant, namespace, funcname)
    resp = requests.get(url)
    pods = json.loads(resp.content)

    return pods


def getpod(tenant: str, namespace: str, podname: str):
    url = "{}/pod/{}/".format(apihostaddr, podname)
    resp = requests.get(url)
    pod = json.loads(resp.content)

    return pod


def getpodaudit(tenant: str, namespace: str, fpname: str, fprevision: int, id: str):
    url = "{}/podauditlog/{}/{}/{}/{}/{}/".format(
        apihostaddr, tenant, namespace, fpname, fprevision, id
    )
    resp = requests.get(url)
    logs = json.loads(resp.content)

    return logs


def GetFailLogs(tenant: str, namespace: str, funcname: str, revision: int):
    url = "{}/faillogs/{}/{}/{}/{}".format(
        apihostaddr, tenant, namespace, funcname, revision
    )
    resp = requests.get(url)
    print("GetFailLogs  ", resp.content)
    fails = json.loads(resp.content)
    return fails


def GetFailLog(tenant: str, namespace: str, funcname: str, revision: int, id: str):
    url = "{}/faillog/{}/{}/{}/{}/{}".format(
        apihostaddr, tenant, namespace, funcname, revision, id
    )
    resp = requests.get(url)
    fail = json.loads(resp.content)
    fail["log"] = fail["log"].replace("\n", "<br>")
    return fail["log"]


def readpodlog(tenant: str, namespace: str, funcname: str, version: int, id: str):
    url = "{}/podlog/{}/{}/{}/{}/{}/".format(
        apihostaddr, tenant, namespace, funcname, version, id
    )
    resp = requests.get(url)
    log = resp.content.decode()
    log = log.replace("\n", "<br>")
    log = log.replace("    ", "&emsp;")
    return log


def getrest(tenant: str, namespace: str, name: str):
    req = "{}/sampleccall/{}/{}/{}/".format(apihostaddr, tenant, namespace, name)
    resp = requests.get(req, stream=False).text
    return resp


@app.route('/text2img', methods=['POST'])
def text2img():
    req = request.get_json()
    
    print("text2img req ", req)
    prompt = req["prompt"]
    tenant = req.get("tenant")
    namespace = req.get("namespace")
    funcname = req.get("funcname")
    
    func = getfunc(tenant, namespace, funcname)

    sample = func["func"]["object"]["spec"]["sample_query"]
    map = sample["body"]

    postreq = {
        "prompt": prompt
    }

    for index, (key, value) in enumerate(map.items()):
        postreq[key] = value

    url = "{}/funccall/{}/{}/{}/{}".format(apihostaddr, tenant, namespace, funcname, sample["path"] )
    
    headers = {
        "Content-Type": "application/json",
    }

    # Stream the response from OpenAI API
    resp = requests.post(url, headers=headers, json=postreq, stream=True)

    # excluded_headers = ['content-encoding', 'content-length', 'transfer-encoding', 'connection']
    excluded_headers = []
    headers = [(name, value) for (name, value) in resp.raw.headers.items() if name.lower() not in excluded_headers]
    return Response(resp.iter_content(1024000), resp.status_code, headers)


@app.route('/generate', methods=['POST'])
def generate():
    # Parse input JSON from the request
    req = request.get_json()
    
    prompt = req["prompt"]
    tenant = req.get("tenant")
    namespace = req.get("namespace")
    funcname = req.get("funcname")
    
    func = getfunc(tenant, namespace, funcname)

    sample = func["func"]["object"]["spec"]["sample_query"]
    map = sample["body"]

    postreq = {
        "prompt": prompt
    }

    isOpenAi = sample["apiType"] == "openai"

    if sample["apiType"] == "llava":
        postreq["image"] = req.get("image")

    for index, (key, value) in enumerate(map.items()):
        postreq[key] = value

    url = "{}/funccall/{}/{}/{}/{}".format(apihostaddr, tenant, namespace, funcname, sample["path"] )
    
    headers = {
        "Content-Type": "application/json",
    }

    # Stream the response from OpenAI API
    response = requests.post(url, headers=headers, json=postreq, stream=True)
    headers = response.headers
    def stream_openai():
        try:
            if response.status_code == 200:
                if isOpenAi:
                    # Iterate over streamed chunks and yield them
                    for data in response.iter_lines():
                        if data:
                            s = data.decode("utf-8")
                            lines = s.split("data:")
                            for line in lines:  
                                if "[DONE]" in line:
                                    continue
                                if len(line) != 0:
                                    # Parse the line as JSON
                                    parsed_line = json.loads(line)
                                    # Extract and print the content delta
                                    if "choices" in parsed_line:
                                        delta = parsed_line["choices"][0]["text"]
                                        yield delta
                                    else:
                                        yield line
                else:
                    for chunk in response.iter_content(chunk_size=1):
                        if chunk:
                            yield(chunk)
            else:
                for chunk in response.iter_content(chunk_size=1):
                    if chunk:
                        yield(chunk)


        except Exception as e:
            yield f"Error: {str(e)}"

    responseheaders = {
        "tcpconn_latency_header": headers["tcpconn_latency_header"],
        "ttft_latency_header": headers["ttft_latency_header"]
    }

    # Return a streaming response
    return Response(stream_openai(), headers = responseheaders, content_type='text/plain')


@app.route("/intro")
def md():
    name = request.args.get("name")
    md_content = read_markdown_file("doc/"+name)
    return render_template(
        "markdown.html", md_content=md_content
    )

@app.route('/doc/<path:filename>')
def route_build_files(filename):
    root_dir = os.path.dirname(os.getcwd()) + "/doc"
    return send_from_directory(root_dir, filename)

@app.route("/funclog")
def funclog():
    namespace = request.args.get("namespace")
    funcId = request.args.get("funcId")
    funcName = request.args.get("funcName")
    log = ReadFuncLog(namespace, funcId)
    output = log.replace("\n", "<br>")
    return render_template(
        "log.html", namespace=namespace, funcId=funcId, funcName=funcName, log=output
    )

@app.route("/")
@app.route("/listfunc")
def ListFunc():
    tenant = request.args.get("tenant")
    namespace = request.args.get("namespace")

    funcs = None
    if tenant is None:
        funcs = listfuncs("", "")
    elif namespace is None:
        funcs = listfuncs(tenant, "")
    else:
        funcs = listfuncs(tenant, namespace)

    count = 0
    gpucount = 0
    vram = 0
    cpu = 0 
    memory = 0
    for func in funcs:
        count += 1
        gpucount += func['func']['object']["spec"]["resources"]["GPU"]["Count"]
        vram += func['func']['object']["spec"]["resources"]["GPU"]["Count"] * func['func']['object']["spec"]["resources"]["GPU"]["vRam"]
        cpu += func['func']['object']["spec"]["resources"]["CPU"]
        memory += func['func']['object']["spec"]["resources"]["Mem"]

    summary = {}
    summary["model_count"] = count
    summary["gpucount"] = gpucount
    summary["vram"] = vram
    summary["cpu"] = cpu
    summary["memory"] = memory
    

    return render_template("func_list.html", funcs=funcs, summary=summary)


@app.route("/listsnapshot")
def ListSnapshot():
    tenant = request.args.get("tenant")
    namespace = request.args.get("namespace")

    snapshots = None
    if tenant is None:
        snapshots = listsnapshots("", "")
    elif namespace is None:
        snapshots = listsnapshots(tenant, "")
    else:
        snapshots = listsnapshots(tenant, namespace)

    return render_template("snapshot_list.html", snapshots=snapshots)


@app.route("/func", methods=("GET", "POST"))
def GetFunc():
    tenant = request.args.get("tenant")
    namespace = request.args.get("namespace")
    name = request.args.get("name")

    func = getfunc(tenant, namespace, name)
    
    sample = func["func"]["object"]["spec"]["sample_query"]
    apiType = sample["apiType"]

    version = func["func"]["object"]["spec"]["version"]
    fails = GetFailLogs(tenant, namespace, name, version)

    # Convert Python dictionary to pretty JSON string
    funcspec = json.dumps(func["func"]["object"]["spec"], indent=4)
    funcspec = funcspec.replace("\n", "<br>")
    funcspec = funcspec.replace("    ", "&emsp;")

    return render_template(
        "func.html",
        tenant=tenant,
        namespace=namespace,
        name=name,
        func=func,
        fails=fails,
        funcspec=funcspec,
        apiType=apiType
    )


# @app.route("/")
@app.route("/listnode")
def ListNode():
    nodes = listnodes()

    for node in nodes:
        gpus = json.dumps(node['object']['resources']['GPUs'], indent=4)
        gpus = gpus.replace("\n", "<br>")
        gpus = gpus.replace("    ", "&emsp;")
        node['object']['resources']['GPUs'] = gpus


    return render_template("node_list.html", nodes=nodes)


@app.route("/node")
def GetNode():
    name = request.args.get("name")
    node = getnode(name)

    nodestr = json.dumps(node["object"], indent=4)
    nodestr = nodestr.replace("\n", "<br>")
    nodestr = nodestr.replace("    ", "&emsp;")

    return render_template("node.html", name=name, node=nodestr)


@app.route("/listpod")
def ListPod():
    tenant = request.args.get("tenant")
    namespace = request.args.get("namespace")

    pods = None
    if tenant is None:
        pods = listpods("", "", "")
    elif namespace is None:
        pods = listpods(tenant, "", "")
    else:
        pods = listpods(tenant, namespace, "")

    return render_template("pod_list.html", pods=pods)


@app.route("/pod")
def GetPod():
    tenant = request.args.get("tenant")
    namespace = request.args.get("namespace")
    podname = request.args.get("name")
    pod = getpod(tenant, namespace, podname)

    funcname = pod["object"]["spec"]["funcname"]
    version = pod["object"]["spec"]["fprevision"]
    id = pod["object"]["spec"]["id"]
    log = readpodlog(tenant, namespace, funcname, version, id)

    audits = getpodaudit(tenant, namespace, funcname, version, id)
    return render_template(
        "pod.html",
        tenant=tenant,
        namespace=namespace,
        podname=podname,
        audits=audits,
        log=log,
    )


@app.route("/failpod")
def GetFailPod():
    tenant = request.args.get("tenant")
    namespace = request.args.get("namespace")
    name = request.args.get("name")
    version = request.args.get("version")
    id = request.args.get("id")

    log = GetFailLog(tenant, namespace, name, version, id)

    audits = getpodaudit(tenant, namespace, name, version, id)
    return render_template(
        "pod.html",
        tenant=tenant,
        namespace=namespace,
        podname=name,
        audits=audits,
        log=log,
    )

def run_http():
    app.run(host='0.0.0.0', port=1250, debug=False)


if __name__ == "__main__":
    http_thread = Thread(target=run_http)
    http_thread.start()

    if tls:
        app.run(host="0.0.0.0", port=1239, ssl_context=('/etc/letsencrypt/live/inferx.net/fullchain.pem', '/etc/letsencrypt/live/inferx.net/privkey.pem'))
        # app.run(host="0.0.0.0", port=1239, ssl_context=('/etc/letsencrypt/live/quarksoft.io/fullchain.pem', '/etc/letsencrypt/live/quarksoft.io/privkey.pem'))
