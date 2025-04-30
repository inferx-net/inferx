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
import functools

from flask import (
    Blueprint,
    Flask,
    jsonify,
    redirect, url_for, session, 
    render_template,
    render_template_string,
    request,
    Response,
    send_from_directory
)

from authlib.integrations.flask_client import OAuth
from authlib.common.security import generate_token 

from threading import Thread

import logging
import sys
import multiprocessing

from werkzeug.middleware.proxy_fix import ProxyFix



# logger = logging.getLogger('gunicorn.error')
# sys.stdout = sys.stderr = logger.handlers[0].stream

app = Flask(__name__)
app.secret_key = os.environ.get("FLASK_SECRET", "supersecret")

def configure_logging():
    if "gunicorn" in multiprocessing.current_process().name.lower():
        logger = logging.getLogger('gunicorn.error')
        if logger.handlers:
            sys.stdout = sys.stderr = logger.handlers[0].stream
            app.logger.info("Redirecting stdout/stderr to Gunicorn logger.")
    else:
        app.logger.info("Running standalone Flask — no stdout/stderr redirection.")

configure_logging()


KEYCLOAK_URL = os.getenv('KEYCLOAK_URL', "http://192.168.0.22:1260/authn")
KEYCLOAK_REALM_NAME = os.getenv('KEYCLOAK_REALM_NAME', "inferx")
KEYCLOAK_CLIENT_ID = os.getenv('KEYCLOAK_CLIENT_ID', "infer_client")
KEYCLOAK_CLIENT_SECRET = os.getenv('KEYCLOAK_CLIENT_SECRET', "M2Dse5531tdtyipZdGizLEeoOVgziQRX")

server_metadata_url = f"{KEYCLOAK_URL}/realms/{KEYCLOAK_REALM_NAME}/.well-known/openid-configuration"

oauth = OAuth(app)
app.wsgi_app = ProxyFix(
    app.wsgi_app, 
    x_for=1,       # Number of trusted proxy hops
    x_proto=1,     # Trust X-Forwarded-Proto (HTTP/HTTPS)
    x_host=1,      # Trust X-Forwarded-Host (external host)
    x_port=1,      # Trust X-Forwarded-Port (external port)
    x_prefix=1  
)

keycloak = oauth.register(
    name='keycloak',
    client_id=KEYCLOAK_CLIENT_ID,
    client_secret=KEYCLOAK_CLIENT_SECRET,
    server_metadata_url=server_metadata_url,
    client_kwargs={
        'scope': 'openid email profile',
        'code_challenge_method': 'S256'  # Enable PKCE
    }
)

tls = False

apihostaddr = "http://localhost:4000"
# apihostaddr = "https://quarksoft.io:4000"

def is_token_expired():
    # Check if token exists and has expiration time
    if 'token' not in session:
        return True
    
    token = session['token']
    return token.get('expires_at', 0) < time.time()

def refresh_token_if_needed():
    if 'token' not in session:
        return False
    
    token = session['token']
    if is_token_expired():
        try:
            new_token = keycloak.fetch_access_token(
                refresh_token=token['refresh_token'],
                grant_type='refresh_token'
            )
            session['token'] = new_token
            session['access_token'] = new_token['access_token']
            return True
        except Exception as e:
            # Handle refresh error (e.g., invalid refresh token)
            print(f"Token refresh failed: {e}")
            session.pop('token', None)
            return False
    return True

def not_require_login(func):
    @functools.wraps(func)
    def wrapper(*args, **kwargs):
        access_token = session.get('access_token', '')
        if access_token == "":
            return func(*args, **kwargs)

        current_path = request.url
        redirect_uri = url_for('login', redirectpath=current_path, _external=True)
        if 'token' not in session:
            return redirect(redirect_uri)
        if is_token_expired() and not refresh_token_if_needed():
            return redirect(redirect_uri)

        return func(*args, **kwargs)
    return wrapper

def require_login(func):
    @functools.wraps(func)
    def wrapper(*args, **kwargs):
        current_path = request.url
        redirect_uri = url_for('login', redirectpath=current_path, _external=True)
        if 'token' not in session:
            return redirect(redirect_uri)
        if is_token_expired() and not refresh_token_if_needed():
            return redirect(redirect_uri)

        return func(*args, **kwargs)
    return wrapper

@app.route('/login')
def login():
    nonce = generate_token(20)
    session['keycloak_nonce'] = nonce
    redirectpath=request.args.get('redirectpath', '')
    redirect_uri = url_for('auth_callback', redirectpath=redirectpath,  _external=True)
    return keycloak.authorize_redirect(
        redirect_uri=redirect_uri,
        nonce=nonce  # Pass nonce to Keycloak
    )

@app.route('/auth/callback')
def auth_callback():
    try:
        # Retrieve token and validate nonce
        token = keycloak.authorize_access_token()
        nonce = session.pop('keycloak_nonce', None)

        redirectpath=request.args.get('redirectpath', '')
    
        if not nonce:
            raise Exception("Missing nonce in session")

        userinfo = keycloak.parse_id_token(token, nonce=nonce)  # Validate nonce
        session['user'] = userinfo
        session['username'] = userinfo.get('preferred_username')
        session['access_token'] = token.get('access_token')
        session['token'] = token
        session['id_token'] = token.get('id_token')

        if redirectpath=='':
            return redirect(url_for('ListFunc'))
        return redirect(redirectpath)
    except Exception as e:
        return f"Authentication failed: {str(e)}", 403

@app.route('/logout')
def logout():
    # Keycloak logout endpoint
    end_session_endpoint = (
        f"{KEYCLOAK_URL}/realms/{KEYCLOAK_REALM_NAME}/protocol/openid-connect/logout"
    )
    
    id_token = session.get('id_token', '')
    # return redirect(end_session_endpoint)

    session.clear()
    # # Redirect to Keycloak to clear SSO session
    return redirect(
        f"{end_session_endpoint}?"
        f"post_logout_redirect_uri={url_for('ListFunc', _external=True)}&"
        f"id_token_hint={id_token}"
    )

def getapkkeys():
    access_token = session.get('token')['access_token']
    # Include the access token in the Authorization header
    headers = {'Authorization': f'Bearer {access_token}'}
    
    url = "{}/apikey/".format(apihostaddr)
    resp = requests.get(url, headers=headers)
    apikeys = json.loads(resp.content)

    return apikeys

@app.route('/apikeys')
@require_login
def apikeys():
    apikeys = getapkkeys()
    tenants = listtenants()
    return render_template(
        "apikey.html", apikeys=apikeys, tenants=tenants
    )

@app.route('/generate_apikeys', methods=['GET'])
@require_login
def generate_apikeys():
    apikeys = getapkkeys()
    return apikeys


@app.route('/apikeys', methods=['PUT'])
@require_login
def create_apikey():
    access_token = session.get('access_token', '')
    if access_token == "":
        headers = {}
    else:
        headers = {'Authorization': f'Bearer {access_token}'}
    req = request.get_json()
    url = "{}/apikey/".format(apihostaddr)
    resp = requests.put(url, headers=headers, json=req)
    return resp

@app.route('/apikeys', methods=['DELETE'])
@require_login
def delete_apikey():
    access_token = session.get('access_token', '')
    if access_token == "":
        headers = {}
    else:
        headers = {'Authorization': f'Bearer {access_token}'}
    req = request.get_json()
    url = "{}/apikey/".format(apihostaddr)
    resp = requests.delete(url, headers=headers, json=req)
    return resp

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
    access_token = session.get('access_token', '')
    if access_token == "":
        headers = {}
    else:
        headers = {'Authorization': f'Bearer {access_token}'}
    url = "{}/functions/{}/{}/".format(apihostaddr, tenant, namespace)
    resp = requests.get(url, headers=headers)
    funcs = json.loads(resp.content)  

    return funcs


def getfunc(tenant: str, namespace: str, funcname: str):
    access_token = session.get('access_token', '')
    if access_token == "":
        headers = {}
    else:
        headers = {'Authorization': f'Bearer {access_token}'}
    url = "{}/function/{}/{}/{}/".format(apihostaddr, tenant, namespace, funcname)
    resp = requests.get(url, headers=headers)
    func = json.loads(resp.content)
    return func


def listsnapshots(tenant: str, namespace: str):
    access_token = session.get('access_token', '')
    if access_token == "":
        headers = {}
    else:
        headers = {'Authorization': f'Bearer {access_token}'}
    url = "{}/snapshots/{}/{}/".format(apihostaddr, tenant, namespace)
    resp = requests.get(url, headers=headers)
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

def listtenants():
    access_token = session.get('access_token', '')
    if access_token == "":
        headers = {}
    else:
        headers = {'Authorization': f'Bearer {access_token}'}
    url = "{}/objects/tenant/system/system/".format(apihostaddr)
    resp = requests.get(url, headers=headers)
    tenants = json.loads(resp.content)

    return tenants

def listnamespaces():
    access_token = session.get('access_token', '')
    if access_token == "":
        headers = {}
    else:
        headers = {'Authorization': f'Bearer {access_token}'}
    url = "{}/objects/namespace///".format(apihostaddr)
    resp = requests.get(url, headers=headers)
    namespaces = json.loads(resp.content)

    return namespaces

def listpods(tenant: str, namespace: str, funcname: str):
    access_token = session.get('access_token', '')
    if access_token == "":
        headers = {}
    else:
        headers = {'Authorization': f'Bearer {access_token}'}
    url = "{}/pods/{}/{}/{}/".format(apihostaddr, tenant, namespace, funcname)
    resp = requests.get(url, headers=headers)
    pods = json.loads(resp.content)

    return pods


def getpod(tenant: str, namespace: str, podname: str):
    access_token = session.get('access_token', '')
    if access_token == "":
        headers = {}
    else:
        headers = {'Authorization': f'Bearer {access_token}'}
    url = "{}/pod/{}/".format(apihostaddr, podname)
    resp = requests.get(url, headers=headers)
    pod = json.loads(resp.content)

    return pod


def getpodaudit(tenant: str, namespace: str, fpname: str, fprevision: int, id: str):
    access_token = session.get('access_token', '')
    if access_token == "":
        headers = {}
    else:
        headers = {'Authorization': f'Bearer {access_token}'}
    url = "{}/podauditlog/{}/{}/{}/{}/{}/".format(
        apihostaddr, tenant, namespace, fpname, fprevision, id
    )
    resp = requests.get(url, headers=headers)
    logs = json.loads(resp.content)

    return logs


def GetFailLogs(tenant: str, namespace: str, funcname: str, revision: int):
    access_token = session.get('access_token', '')
    if access_token == "":
        headers = {}
    else:
        headers = {'Authorization': f'Bearer {access_token}'}
    url = "{}/faillogs/{}/{}/{}/{}".format(
        apihostaddr, tenant, namespace, funcname, revision
    )
    resp = requests.get(url, headers=headers)
    fails = json.loads(resp.content)
    return fails


def GetFailLog(tenant: str, namespace: str, funcname: str, revision: int, id: str):
    access_token = session.get('access_token', '')
    if access_token == "":
        headers = {}
    else:
        headers = {'Authorization': f'Bearer {access_token}'}
    url = "{}/faillog/{}/{}/{}/{}/{}".format(
        apihostaddr, tenant, namespace, funcname, revision, id
    )
    resp = requests.get(url)
    fail = json.loads(resp.content)
    fail["log"] = fail["log"].replace("\n", "<br>")
    return fail["log"]


def readpodlog(tenant: str, namespace: str, funcname: str, version: int, id: str):
    access_token = session.get('access_token', '')
    if access_token == "":
        headers = {}
    else:
        headers = {'Authorization': f'Bearer {access_token}'}
    url = "{}/podlog/{}/{}/{}/{}/{}/".format(
        apihostaddr, tenant, namespace, funcname, version, id
    )
    resp = requests.get(url, headers=headers)
    log = resp.content.decode()
    log = log.replace("\n", "<br>")
    log = log.replace("    ", "&emsp;")
    return log


def getrest(tenant: str, namespace: str, name: str):
    access_token = session.get('access_token', '')
    if access_token == "":
        headers = {}
    else:
        headers = {'Authorization': f'Bearer {access_token}'}
    req = "{}/sampleccall/{}/{}/{}/".format(apihostaddr, tenant, namespace, name)
    resp = requests.get(req, stream=False, headers=headers).text
    return resp


@app.route('/text2img', methods=['POST'])
@not_require_login
def text2img():
    access_token = session.get('access_token', '')
    if access_token == "":
        headers = {
            "Content-Type": "application/json",
        }
    else:
        headers = {
            'Authorization': f'Bearer {access_token}',
            "Content-Type": "application/json",
        }
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

    # Stream the response from OpenAI API
    resp = requests.post(url, headers=headers, json=postreq, stream=True)

    # excluded_headers = ['content-encoding', 'content-length', 'transfer-encoding', 'connection']
    excluded_headers = []
    headers = [(name, value) for (name, value) in resp.raw.headers.items() if name.lower() not in excluded_headers]
    return Response(resp.iter_content(1024000), resp.status_code, headers)

@app.route('/generate_tenants', methods=['GET'])
@require_login
def generate_tenants():
    tenants = listtenants()
    print("tenants ", tenants)
    return tenants

@app.route('/generate_namespaces', methods=['GET'])
@require_login
def generate_namespaces():
    namespaces = listnamespaces()
    print("namespaces ", namespaces)
    return namespaces

@app.route('/generate_funcs', methods=['GET'])
@require_login
def generate_funcs():
    funcs = listfuncs("", "")
    return funcs

@app.route('/generate', methods=['POST'])
@not_require_login
def generate():
    access_token = session.get('access_token', '')
    if access_token == "":
        headers = {
            "Content-Type": "application/json",
        }
    else:
        headers = {
            'Authorization': f'Bearer {access_token}',
            "Content-Type": "application/json",
        }
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



def stream_response(response):
    try:
        for chunk in response.iter_content(chunk_size=128):
            yield chunk
    finally:
        response.close()

@app.route('/proxy/<path:path>', methods=['GET', 'POST', 'PUT', 'DELETE', 'PATCH', 'OPTIONS'])
@not_require_login
def proxy(path):
    access_token = session.get('access_token', '')
    headers = {key: value for key, value in request.headers if key.lower() != 'host'}
    if access_token != "":
        headers["Authorization"] = f'Bearer {access_token}'
    
    # Construct the full URL for the backend request
    url = f"{apihostaddr}/{path}"

    print("proxy path ", path)
    try:
        resp = requests.request(
            method=request.method,
            url=url,
            headers=headers,
            data=request.get_data(),
            cookies=request.cookies,
            allow_redirects=False,
            timeout=60,
            stream=True
        )
    except requests.exceptions.RequestException as e:
        return Response(f"Error connecting to backend server: {e}", status=502)
    
    # Exclude hop-by-hop headers as per RFC 2616 section 13.5.1
    excluded_headers = ['content-encoding', 'transfer-encoding', 'connection']
    headers = [(name, value) for name, value in resp.raw.headers.items() if name.lower() not in excluded_headers]
    
    print("response ", resp.status_code, path)

    # Create a Flask response object with the backend server's response
    response = Response(stream_response(resp), resp.status_code, headers)
    return response

@app.route('/proxy1/<path:path>', methods=['GET', 'POST', 'PUT', 'DELETE', 'PATCH', 'OPTIONS'])
@require_login
def proxy1(path):
    access_token = session.get('access_token', '')
    headers = {key: value for key, value in request.headers if key.lower() != 'host'}
    if access_token != "":
        headers["Authorization"] = f'Bearer {access_token}'
    
    # Construct the full URL for the backend request
    url = f"{apihostaddr}/{path}"

    print("proxy path ", path)
    try:
        resp = requests.request(
            method=request.method,
            url=url,
            headers=headers,
            params=request.args,
            data=request.get_data(),
            cookies=request.cookies,
            allow_redirects=False,
            timeout=60,
            stream=False
        )
    except requests.exceptions.RequestException as e:
        print("error ....")
        return Response(f"Error connecting to backend server: {e}", status=502, mimetype='text/plain')
    
    print("response ", resp.status_code, path, resp.content)

    response = Response(resp.content, resp.status_code)
    # for name, value in resp.headers.items():
    #     if name.lower() not in ['content-encoding', 'transfer-encoding', 'connection']:
    #         response.headers[name] = value

    return response
    


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
@not_require_login
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
@not_require_login
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
@not_require_login
def GetFunc():
    tenant = request.args.get("tenant")
    namespace = request.args.get("namespace")
    name = request.args.get("name")

    func = getfunc(tenant, namespace, name)
    print("func ", func)
    
    sample = func["func"]["object"]["spec"]["sample_query"]
    map = sample["body"]
    apiType = sample["apiType"]
    isAdmin = func["isAdmin"]

    version = func["func"]["object"]["spec"]["version"]
    fails = GetFailLogs(tenant, namespace, name, version)

    # Convert Python dictionary to pretty JSON string
    funcspec = json.dumps(func["func"]["object"]["spec"], indent=4)

    return render_template(
        "func.html",
        tenant=tenant,
        namespace=namespace,
        name=name,
        func=func,
        fails=fails,
        funcspec=funcspec,
        apiType=apiType,
        map=map,
        isAdmin=isAdmin,
        path=sample["path"]
    )


# @app.route("/")
@app.route("/listnode")
@not_require_login
def ListNode():
    nodes = listnodes()

    for node in nodes:
        gpus = json.dumps(node['object']['resources']['GPUs'], indent=4)
        gpus = gpus.replace("\n", "<br>")
        gpus = gpus.replace("    ", "&emsp;")
        node['object']['resources']['GPUs'] = gpus


    return render_template("node_list.html", nodes=nodes)


@app.route("/node")
@not_require_login
def GetNode():
    name = request.args.get("name")
    node = getnode(name)

    nodestr = json.dumps(node["object"], indent=4)
    nodestr = nodestr.replace("\n", "<br>")
    nodestr = nodestr.replace("    ", "&emsp;")

    return render_template("node.html", name=name, node=nodestr)


@app.route("/listpod")
@not_require_login
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
@not_require_login
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
@not_require_login
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
    app.run(host='0.0.0.0', port=1250, debug=True)


if __name__ == "__main__":
    if tls:
        http_thread = Thread(target=run_http)
        http_thread.start()
        app.run(host="0.0.0.0", port=1239, ssl_context=('/etc/letsencrypt/live/inferx.net/fullchain.pem', '/etc/letsencrypt/live/inferx.net/privkey.pem'))
        # app.run(host="0.0.0.0", port=1239, ssl_context=('/etc/letsencrypt/live/quarksoft.io/fullchain.pem', '/etc/letsencrypt/live/quarksoft.io/privkey.pem'))
    else:
        app.run(host='0.0.0.0', port=1250, debug=True)
