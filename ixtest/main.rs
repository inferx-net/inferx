#![allow(dead_code)]
#![allow(non_snake_case)]
#![allow(non_camel_case_types)]
#![allow(non_upper_case_globals)]

use std::env;

use ixshare::na;
use ixshare::ixmeta;

fn usage() {
    eprintln!("Usage:");
    eprintln!("  ixtest <na_addr> create_func_pod [tenant] [namespace] [funcname] [id] [fprevision] [create_type] [funcspec_json]");
    eprintln!("  ixtest <na_addr> cr_swapout [container_name]");
    eprintln!("  ixtest <na_addr> cr_swapin [container_name] [gpu_map]");
    eprintln!("  ixtest <na_addr> cr_swap_test [container_name] [port] [rounds] [gpu_map]");
    eprintln!("  ixtest <na_addr> list_pods [tenant] [namespace]");
    eprintln!("  ixtest <na_addr> get_pod [tenant] [namespace] [name]");
    eprintln!("  ixtest <na_addr> read_pod_log [tenant] [namespace] [funcname] [fprevision] [id]");
    eprintln!("  ixtest <na_addr> remove_snapshot [funckey]");
    eprintln!("  ixtest <na_addr> terminate_pod [tenant] [namespace] [funcname] [fprevision] [id]");
    eprintln!();
    eprintln!("  gpu_map: comma-separated vgpu->pgpu mapping, e.g. '1,0' to swap GPUs; empty for no migration");
}

fn extract_host(addr: &str) -> String {
    let stripped = addr.strip_prefix("http://").or_else(|| addr.strip_prefix("https://")).unwrap_or(addr);
    if let Some(colon) = stripped.find(':') {
        stripped[..colon].to_string()
    } else {
        stripped.to_string()
    }
}

const STATE_SVC_PORT: u16 = 1236;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    simple_logging::log_to_stderr(log::LevelFilter::Info);

    let args: Vec<String> = env::args().collect();
    if args.len() < 3 {
        usage();
        std::process::exit(1);
    }

    let na_addr = &args[1];
    let command = &args[2];

    match command.as_str() {
        "create_func_pod" => {
            let tenant = args.get(3).map(|s| s.as_str()).unwrap_or("public");
            let namespace = args.get(4).map(|s| s.as_str()).unwrap_or("default");
            let funcname = args.get(5).map(|s| s.as_str()).unwrap_or("test-func");
            let id = args.get(6).map(|s| s.as_str()).unwrap_or("1");
            let fprevision: i64 = args.get(7).map(|s| s.parse().unwrap_or(1)).unwrap_or(1);
            let create_type_str = args.get(8).map(|s| s.as_str()).unwrap_or("normal");
            let funcspec_path = args.get(9).map(|s| s.as_str());

            let create_type = match create_type_str.to_lowercase().as_str() {
                "snapshot" => na::CreatePodType::Snapshot,
                "restore" => na::CreatePodType::Restore,
                "crcontainer" => na::CreatePodType::CrContainer,
                _ => na::CreatePodType::Normal,
            };

            let funcspec_str = if let Some(path) = funcspec_path {
                if path.starts_with('{') {
                    path.to_string()
                } else {
                    std::fs::read_to_string(path)?
                }
            } else {
                serde_json::json!({
                    "image": "localhost/vllm/vllm-openai:v0.20.0-cu129",
                    "commands": [],
                    "envs": [],
                    "mounts": [],
                    "endpoint": {
                        "port": 8000,
                        "probe": "/health",
                        "schema": "http",
                        "probe_timeout": 1000,
                        "probetype": "Prompt"
                    },
                    "version": 1,
                    "model_type": "Public",
                    "entrypoint": [],
                    "resources": {
                        "CPU": 1000,
                        "Mem": 8000,
                        "ReadyMem": 0,
                        "CacheMem": 0,
                        "GPU": {
                            "Type": "",
                            "Count": 1,
                            "vRam": 0,
                            "contextCount": 1
                        }
                    },
                    "standby": {
                        "gpu": "File",
                        "pageable": "File",
                        "pinned": "File"
                    },
                    "sample_query": {
                        "apiType": "text2text",
                        "path": "/v1/completions",
                        "prompt": "Hello",
                        "body": {}
                    },
                    "policy": {"Obj": {}},
                    "mountfiles": []
                }).to_string()
            };

            let gpu_id: i32 = {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&funcspec_str) {
                    v.get("gpu").and_then(|x| x.as_str())
                        .and_then(|s| s.split(',').next())
                        .and_then(|s| s.trim().parse::<i32>().ok())
                        .unwrap_or(0)
                } else { 0 }
            };

            let alloc_resources = serde_json::json!({
                "nodename": "",
                "CPU": 1000,
                "Mem": 8000,
                "CacheMem": 0,
                "GPUType": "NVIDIA RTX A6000",
                "GPUs": {
                    "totalSlotCnt": 170,
                    "map": {gpu_id.to_string(): {"contextCnt": 1, "slotCnt": 170, "ncclCnt": 1}},
                    "slotSize": 268435456
                },
                "MaxContextPerGPU": 1
            });

            let resource_quota = serde_json::json!({
                "nodename": "",
                "CPU": 1000,
                "Mem": 8000,
                "CacheMem": 0,
                "GPUType": "NVIDIA RTX A6000",
                "GPUs": {
                    "totalSlotCnt": 170,
                    "map": {gpu_id.to_string(): {"contextCnt": 1, "slotCnt": 170, "ncclCnt": 1}},
                    "slotSize": 268435456
                },
                "MaxContextPerGPU": 1
            });

            let mut client =
                na::node_agent_service_client::NodeAgentServiceClient::connect(na_addr.to_string())
                    .await?;

            let request = tonic::Request::new(na::CreateFuncPodReq {
                tenant: tenant.to_string(),
                namespace: namespace.to_string(),
                funcname: funcname.to_string(),
                fprevision: fprevision,
                id: id.to_string(),
                labels: Vec::new(),
                annotations: Vec::new(),
                create_type: create_type.into(),
                funcspec: funcspec_str,
                alloc_resources: alloc_resources.to_string(),
                resource_quota: resource_quota.to_string(),
                terminate_pods: Vec::new(),
            });

            println!("Sending CreateFuncPod to {} ...", na_addr);
            let response = client.create_func_pod(request).await?;
            let resp = response.into_inner();

            if resp.error.is_empty() {
                println!("Success! IP address: {}", resp.ipaddress);
            } else {
                println!("Error: {}", resp.error);
            }
        }
        "cr_swapout" => {
            let container_name = args.get(3).map(|s| s.as_str()).unwrap_or("swap");

            let mut client =
                na::node_agent_service_client::NodeAgentServiceClient::connect(na_addr.to_string())
                    .await?;

            let request = tonic::Request::new(na::CrSwapoutReq {
                container_name: container_name.to_string(),
            });

            println!("Sending CrSwapout to {} (container={}) ...", na_addr, container_name);
            let response = client.cr_swapout(request).await?;
            let resp = response.into_inner();

            if resp.error.is_empty() {
                println!("Success");
            } else {
                println!("Error: {}", resp.error);
            }
        }
        "cr_swapin" => {
            let container_name = args.get(3).map(|s| s.as_str()).unwrap_or("swap");
            let gpu_map = args.get(4).map(|s| s.as_str()).unwrap_or("");

            let mut client =
                na::node_agent_service_client::NodeAgentServiceClient::connect(na_addr.to_string())
                    .await?;

            let request = tonic::Request::new(na::CrSwapinReq {
                container_name: container_name.to_string(),
                gpu_map: gpu_map.to_string(),
            });

            println!("Sending CrSwapin to {} (container={}, gpu_map='{}') ...", na_addr, container_name, gpu_map);
            let response = client.cr_swapin(request).await?;
            let resp = response.into_inner();

            if resp.error.is_empty() {
                println!("Success");
            } else {
                println!("Error: {}", resp.error);
            }
        }
        "cr_swap_test" => {
            let container_name = args.get(3).map(|s| s.as_str()).unwrap_or("swap");
            let port_str = args.get(4).map(|s| s.as_str()).unwrap_or("8001");
            let rounds: u32 = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(3);
            let gpu_map = args.get(6).map(|s| s.as_str()).unwrap_or("");
            let port: u16 = port_str.parse().unwrap_or(8001);

            let mut client =
                na::node_agent_service_client::NodeAgentServiceClient::connect(na_addr.to_string())
                    .await?;

            // Resolve pod name for kubectl exec verification
            let all_pods = std::process::Command::new("sudo")
                .args(["kubectl", "get", "pods", "-o", "jsonpath={.items[*].metadata.name}"])
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                .unwrap_or_default();
            let pod_name = all_pods
                .split_whitespace()
                .find(|n| n.starts_with("nodeagent-cr"))
                .map(|s| s.to_string())
                .unwrap_or_default();

            println!("=== cr_swap_test: container={} port={} rounds={} gpu_map='{}' pod={} ===",
                container_name, port, rounds, gpu_map, pod_name);

            let http_get = |path: &str| -> Option<String> {
                if pod_name.is_empty() { return None; }
                let out = std::process::Command::new("sudo")
                    .args(["kubectl", "exec", &pod_name, "--", "curl", "-sf", "--max-time", "10",
                           &format!("http://localhost:{}{}", port, path)])
                    .output()
                    .ok()?;
                if out.status.success() {
                    Some(String::from_utf8_lossy(&out.stdout).to_string())
                } else {
                    None
                }
            };
            let http_post = |path: &str, body: &str| -> Option<String> {
                if pod_name.is_empty() { return None; }
                let out = std::process::Command::new("sudo")
                    .args(["kubectl", "exec", &pod_name, "--", "curl", "-sf", "--max-time", "30",
                           "-H", "Content-Type: application/json",
                           "-d", body,
                           &format!("http://localhost:{}{}", port, path)])
                    .output()
                    .ok()?;
                if out.status.success() {
                    Some(String::from_utf8_lossy(&out.stdout).to_string())
                } else {
                    None
                }
            };

            // Initial swapin (container starts SwappedOut after first-time setup)
            let query_body = format!(r#"{{"model":"{}","messages":[{{"role":"user","content":"hi"}}],"max_tokens":5}}"#, container_name);
            println!("[init] swapin...");
            let resp = client.cr_swapin(tonic::Request::new(na::CrSwapinReq {
                container_name: container_name.to_string(),
                gpu_map: gpu_map.to_string(),
            })).await?;
            let r = resp.into_inner();
            if !r.error.is_empty() {
                println!("[init] swapin FAILED: {}", r.error);
                std::process::exit(1);
            }
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

            // Baseline query
            println!("[baseline] querying model...");
            match http_post("/v1/chat/completions", &query_body) {
                Some(resp) => println!("[baseline] OK: {}", resp),
                None => {
                    println!("[baseline] FAILED: cannot reach model");
                    std::process::exit(1);
                }
            }

            for round in 1..=rounds {
                println!("\n--- round {} ---", round);

                // swapout
                println!("[round {}] swapout...", round);
                let resp = client.cr_swapout(tonic::Request::new(na::CrSwapoutReq {
                    container_name: container_name.to_string(),
                })).await?;
                let r = resp.into_inner();
                if !r.error.is_empty() {
                    println!("[round {}] swapout FAILED: {}", round, r.error);
                    std::process::exit(1);
                }
                println!("[round {}] swapout OK", round);

                // verify sleeping
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                match http_get("/is_sleeping") {
                    Some(body) => {
                        println!("[round {}] is_sleeping: {}", round, body);
                        if !body.contains("\"is_sleeping\":true") {
                            println!("[round {}] WARNING: container not sleeping after swapout", round);
                        }
                    }
                    None => println!("[round {}] is_sleeping: (unreachable)", round),
                }

                // swapin
                println!("[round {}] swapin (gpu_map='{}')...", round, gpu_map);
                let resp = client.cr_swapin(tonic::Request::new(na::CrSwapinReq {
                    container_name: container_name.to_string(),
                    gpu_map: gpu_map.to_string(),
                })).await?;
                let r = resp.into_inner();
                if !r.error.is_empty() {
                    println!("[round {}] swapin FAILED: {}", round, r.error);
                    std::process::exit(1);
                }
                println!("[round {}] swapin OK", round);

                // verify serving
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                println!("[round {}] querying model...", round);
                match http_post("/v1/chat/completions", &query_body) {
                    Some(resp) => println!("[round {}] OK: {}", round, resp),
                    None => {
                        println!("[round {}] query FAILED", round);
                        std::process::exit(1);
                    }
                }
            }

            println!("\n=== cr_swap_test PASSED: {}/{} rounds ===", rounds, rounds);
        }
        "list_pods" => {
            let tenant = args.get(3).map(|s| s.as_str()).unwrap_or("");
            let namespace = args.get(4).map(|s| s.as_str()).unwrap_or("");

            let host = extract_host(na_addr);
            let state_svc_addr = format!("http://{}:{}", host, STATE_SVC_PORT);

            let mut client = ixmeta::ix_meta_service_client::IxMetaServiceClient::connect(state_svc_addr.clone())
                .await?;

            let request = tonic::Request::new(ixmeta::ListRequestMessage {
                obj_type: "pod".to_string(),
                tenant: tenant.to_string(),
                namespace: namespace.to_string(),
                revision: 0,
                label_selector: String::new(),
                field_selector: String::new(),
            });

            println!("Listing pods from StateSvc at {} ...", state_svc_addr);
            let response = client.list(request).await?;
            let resp = response.into_inner();

            if !resp.error.is_empty() {
                println!("Error: {}", resp.error);
            } else {
                println!("Found {} pod(s):", resp.objs.len());
                for obj in &resp.objs {
                    let data: serde_json::Value = serde_json::from_str(&obj.data).unwrap_or(serde_json::json!({"raw": obj.data}));
                    let state = data.get("status")
                        .and_then(|s| s.get("state"))
                        .and_then(|s| s.as_str())
                        .unwrap_or("?");
                    let funcname = data.get("spec")
                        .and_then(|s| s.get("funcname"))
                        .and_then(|f| f.as_str())
                        .unwrap_or("?");
                    println!("  {}/{}/{}  state={}  funcname={}", obj.tenant, obj.namespace, obj.name, state, funcname);
                }
            }
        }
        "get_pod" => {
            let tenant = args.get(3).map(|s| s.as_str()).unwrap_or("public");
            let namespace = args.get(4).map(|s| s.as_str()).unwrap_or("default");
            let name = args.get(5).map(|s| s.as_str()).unwrap_or("");

            if name.is_empty() {
                eprintln!("get_pod requires a pod name");
                std::process::exit(1);
            }

            let host = extract_host(na_addr);
            let state_svc_addr = format!("http://{}:{}", host, STATE_SVC_PORT);

            let mut client = ixmeta::ix_meta_service_client::IxMetaServiceClient::connect(state_svc_addr.clone())
                .await?;

            let request = tonic::Request::new(ixmeta::GetRequestMessage {
                obj_type: "pod".to_string(),
                tenant: tenant.to_string(),
                namespace: namespace.to_string(),
                name: name.to_string(),
                revision: 0,
            });

            println!("Getting pod {}/{}/{} from StateSvc at {} ...", tenant, namespace, name, state_svc_addr);
            let response = client.get(request).await?;
            let resp = response.into_inner();

            if !resp.error.is_empty() {
                println!("Error: {}", resp.error);
            } else if let Some(obj) = resp.obj {
                let data: serde_json::Value = serde_json::from_str(&obj.data).unwrap_or(serde_json::json!({"raw": obj.data}));
                println!("{}", serde_json::to_string_pretty(&data).unwrap_or(obj.data));
            } else {
                println!("Pod not found");
            }
        }
        "read_pod_log" => {
            let tenant = args.get(3).map(|s| s.as_str()).unwrap_or("public");
            let namespace = args.get(4).map(|s| s.as_str()).unwrap_or("default");
            let funcname = args.get(5).map(|s| s.as_str()).unwrap_or("");
            let fprevision: i64 = args.get(6).and_then(|s| s.parse().ok()).unwrap_or(1);
            let id = args.get(7).map(|s| s.as_str()).unwrap_or("1");

            if funcname.is_empty() {
                eprintln!("read_pod_log requires a funcname");
                std::process::exit(1);
            }

            let mut client =
                na::node_agent_service_client::NodeAgentServiceClient::connect(na_addr.to_string())
                    .await?;

            let request = tonic::Request::new(na::ReadPodLogReq {
                tenant: tenant.to_string(),
                namespace: namespace.to_string(),
                funcname: funcname.to_string(),
                fprevision,
                id: id.to_string(),
            });

            println!("Reading pod log for {}/{}/{}/{}/{} ...", tenant, namespace, funcname, fprevision, id);
            let response = client.read_pod_log(request).await?;
            let resp = response.into_inner();

            if !resp.error.is_empty() {
                println!("Error: {}", resp.error);
            } else {
                println!("{}", resp.log);
            }
        }
        "remove_snapshot" => {
            let funckey = args.get(3).map(|s| s.as_str()).unwrap_or("");

            if funckey.is_empty() {
                eprintln!("remove_snapshot requires a funckey");
                std::process::exit(1);
            }

            let mut client =
                na::node_agent_service_client::NodeAgentServiceClient::connect(na_addr.to_string())
                    .await?;

            let request = tonic::Request::new(na::RemoveSnapshotReq {
                funckey: funckey.to_string(),
            });

            println!("Removing snapshot/funckey={} ...", funckey);
            let response = client.remove_snapshot(request).await?;
            let resp = response.into_inner();

            if !resp.error.is_empty() {
                println!("Error: {}", resp.error);
            } else {
                println!("Success");
            }
        }
        "terminate_pod" => {
            let tenant = args.get(3).map(|s| s.as_str()).unwrap_or("public");
            let namespace = args.get(4).map(|s| s.as_str()).unwrap_or("default");
            let funcname = args.get(5).map(|s| s.as_str()).unwrap_or("");
            let fprevision: i64 = args.get(6).and_then(|s| s.parse().ok()).unwrap_or(1);
            let id = args.get(7).map(|s| s.as_str()).unwrap_or("1");

            if funcname.is_empty() {
                eprintln!("terminate_pod requires a funcname");
                std::process::exit(1);
            }

            let mut client =
                na::node_agent_service_client::NodeAgentServiceClient::connect(na_addr.to_string())
                    .await?;

            let request = tonic::Request::new(na::TerminatePodReq {
                tenant: tenant.to_string(),
                namespace: namespace.to_string(),
                funcname: funcname.to_string(),
                fprevision,
                id: id.to_string(),
            });

            println!("Terminating pod {}/{}/{}/{}/{} ...", tenant, namespace, funcname, fprevision, id);
            let response = client.terminate_pod(request).await?;
            let resp = response.into_inner();

            if !resp.error.is_empty() {
                println!("Error: {}", resp.error);
            } else {
                println!("Success");
            }
        }
        _ => {
            eprintln!("Unknown command: {}", command);
            usage();
            std::process::exit(1);
        }
    }

    Ok(())
}