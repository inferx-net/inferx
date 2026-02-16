use std::{
    collections::BTreeMap, env, process, sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    }, time::{Duration, Instant}
};

use reqwest::Client;
use serde_json::json;
use serde_json::Value;
use tokio::sync::Mutex;

pub static REQ_COUNT: AtomicUsize = AtomicUsize::new(0);
const DEFAULT_PROMPT: &str =
    "Can you provide ways to eat combinations of bananas and dragonfruits?";
const DEFAULT_MAX_TOKENS: u32 = 20;
const DEFAULT_GATEWAY_ENDPOINT: &str = "http://localhost:31501"; //"http://localhost:4000";
const DEFAULT_TENANT: &str = "public";

#[tokio::main]
async fn main() {
    let mut args = env::args();
    let program = args
        .next()
        .unwrap_or_else(|| String::from("ixtest"));
    let input_args: Vec<String> = args.collect();

    if input_args.is_empty() {
        fatal(&program, "missing required arguments");
    }

    let (
        concurrency,
        duration_secs,
        modelalias,
        model,
    show_outputs,
    prompt,
    max_tokens,
    max_requests,
    gateway_endpoint,
    token,
    tenant,
    ) = if input_args.iter().all(|arg| arg.contains('=')) {
        parse_key_value_args(&program, &input_args)
    } else {
        parse_positional_args(&program, &input_args)
    };

    if duration_secs == 0 && max_requests.is_none() {
        fatal(
            &program,
            "duration must be greater than zero when max_requests is not provided",
        );
    }

    run_hey(
        concurrency,
        duration_secs,
        &modelalias,
        &model,
        show_outputs,
        &prompt,
        max_tokens,
        max_requests,
        &gateway_endpoint,
        token,
        &tenant,
    )
    .await;
}

async fn run_hey(
    concurrency: usize,
    duration_secs: u64,
    modelalias: &str,
    model: &str,
    show_outputs: bool,
    prompt: &str,
    max_tokens: u32,
    max_requests: Option<usize>,
    gateway_endpoint: &str,
    token: Option<String>,
    tenant: &str,
) {
    let base = gateway_endpoint.trim_end_matches('/');
    let tenant = tenant.trim_matches('/');
    let url = format!("{}/funccall/{}/{}/v1/completions", base, tenant, modelalias);
    println!(
        "=== Running hey for model: {} (c={}, z={}s) ===",
        modelalias, concurrency, duration_secs
    );

    let client = Client::new();
    let stop_time = if duration_secs == 0 {
        None
    } else {
        Some(Instant::now() + Duration::from_secs(duration_secs))
    };

    let success = Arc::new(AtomicUsize::new(0));
    let fail = Arc::new(AtomicUsize::new(0));
    let mismatch = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();
    let token = token.filter(|t| !t.is_empty());

    let joined: Arc<Mutex<BTreeMap<String, i32>>> = Arc::new(Mutex::new(BTreeMap::new()));
    for _ in 0..concurrency {
        let client = client.clone();
        let url = url.clone();
        let success = success.clone();
        let fail = fail.clone();
        let mismatch = mismatch.clone();

        let clone = joined.clone();
        let model = model.to_string();
        let prompt = prompt.to_string();
        let max_tokens = max_tokens;
        let stop_time = stop_time;
        let max_requests = max_requests;
        let token = token.clone();
        handles.push(tokio::spawn(async move {
            let mut first = None;
            let mut output: BTreeMap<String, i32> = BTreeMap::new();

            loop {
                if let Some(stop) = stop_time {
                    if Instant::now() >= stop {
                        break;
                    }
                }

                let req_id = if let Some(limit) = max_requests {
                    let current = REQ_COUNT.fetch_add(1, Ordering::SeqCst);
                    if current >= limit {
                        break;
                    }
                    current
                } else {
                    REQ_COUNT.fetch_add(1, Ordering::SeqCst)
                };

                let body = json!({
                    "prompt": prompt,
                    "max_tokens": max_tokens.to_string(),
                    "model": model,
                    "stream": "false",
                    "temperature": "0",
                    "top_p": 1.0,
                    "seed": 0
                });

                let mut req = client.post(&url).json(&body);
                if let Some(ref token) = token {
                    req = req.bearer_auth(token);
                }

                match req.send().await {
                    Ok(resp) => {
                        let status = resp.status();
                        match resp.text().await {
                            Ok(resp) if status.is_success() => {
                                // println!("resp is {}", &resp);
                                let v: Value = serde_json::from_str(&resp).unwrap();

                                // Safely extract the text field
                                let text = if let Some(text) = v["choices"]
                                    .as_array()
                                    .and_then(|arr| arr.first())
                                    .and_then(|choice| choice["text"].as_str())
                                {
                                    text
                                } else {
                                    println!("Failed to extract text: {}", &resp);
                                    mismatch.fetch_add(1, Ordering::Relaxed);
                                    continue;
                                };

                                match output.get_mut(text) {
                                    Some(v) => {
                                        *v += 1;
                                    }   
                                    None => {
                                        output.insert(text.to_owned(), 1);
                                    }
                                }

                                let text = text.to_owned();

                                if first.is_none() {
                                    first = Some(text);
                                    success.fetch_add(1, Ordering::Relaxed);
                                } else if Some(&text) == first.as_ref() {
                                    success.fetch_add(1, Ordering::Relaxed);
                                } else {
                                    // println!("mismatch expect {:?}\n actual {:?}", first.as_ref(), &text);
                                    mismatch.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                            Ok(resp) => {
                                println!("status is {:?}, model is {} reqid is {}, resp is {}", &status, &model, req_id, resp);
                                fail.fetch_add(1, Ordering::Relaxed);
                            }
                            Err(e) => {
                                println!("resp.text error is {:?}", &e);
                                fail.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                    Err(e) => {
                        println!("client.post error is {:?}", &e);
                        fail.fetch_add(1, Ordering::Relaxed);
                    }
                }

                // small pause between requests
                // sleep(Duration::from_millis(10)).await;
            }

            let mut lock = clone.lock().await;
            for (k, &c) in &output {
                match lock.get_mut(k) {
                    Some(v) => {
                        *v += c;
                    }   
                    None => {
                        lock.insert(k.to_owned(), c);
                    }
                }
            }


        }));
    }


    for h in handles {
        let _ = h.await;
    }

    if show_outputs {
        let mut aggregated: Vec<(String, i32)> = {
            let lock = joined.lock().await;
            lock.iter()
                .map(|(text, &count)| (text.clone(), count))
                .collect()
        };

        // sort by count asc, and then text asc for deterministic output
        aggregated.sort_by_key(|(text, count)| (*count, text.clone()));

        if aggregated.is_empty() {
            println!("output text: <empty>");
        } else {
            println!("output text (sorted by count desc):");
            for (text, count) in aggregated {
                println!("{}: {}", count, text);
            }
        }
    }

    println!(
        "[200 OK]:\t{}\t[mismatch]:\t\t{}\t[fail]:\t\t\t{}\t\n",
        success.load(Ordering::Relaxed) + mismatch.load(Ordering::Relaxed),
        mismatch.load(Ordering::Relaxed),
        fail.load(Ordering::Relaxed),
    );

    println!(
        "===================== model: {} Done ======================\n\n\n",
        modelalias
    );
    
    
}

fn parse_positional_args(
    program: &str,
    args: &[String],
) -> (
    usize,
    u64,
    String,
    String,
    bool,
    String,
    u32,
    Option<usize>,
    String,
    Option<String>,
    String,
) {
    if args.len() < 3 || args.len() > 7 {
        fatal(
            program,
            "expected 3 to 7 positional arguments",
        );
    }

    let concurrency: usize = args[0]
        .parse()
        .unwrap_or_else(|_| fatal(program, "invalid concurrency value"));
    let duration_secs: u64 = args[1]
        .parse()
        .unwrap_or_else(|_| fatal(program, "invalid duration seconds value"));

    let (modelalias, model, max_requests, token, tenant) = match args.len() {
        3 => {
            let model = args[2].clone();
            (model.clone(), model, None, None, DEFAULT_TENANT.to_owned())
        }
        4 => {
            if let Some(max) = try_parse_positive_usize(&args[3]) {
                let model = args[2].clone();
                (model.clone(), model, Some(max), None, DEFAULT_TENANT.to_owned())
            } else {
                (
                    args[2].clone(),
                    args[3].clone(),
                    None,
                    None,
                    DEFAULT_TENANT.to_owned(),
                )
            }
        }
        5 => {
            let max = parse_positive_usize(program, "max_requests", &args[4]);
            (
                args[2].clone(),
                args[3].clone(),
                Some(max),
                None,
                DEFAULT_TENANT.to_owned(),
            )
        }
        6 => {
            let max = parse_positive_usize(program, "max_requests", &args[4]);
            (
                args[2].clone(),
                args[3].clone(),
                Some(max),
                Some(args[5].clone()),
                DEFAULT_TENANT.to_owned(),
            )
        }
        7 => {
            let max = parse_positive_usize(program, "max_requests", &args[4]);
            (
                args[2].clone(),
                args[3].clone(),
                Some(max),
                Some(args[5].clone()),
                args[6].clone(),
            )
        }
        _ => unreachable!(),
    };

    (
        concurrency,
        duration_secs,
        modelalias,
        model,
        false,
        DEFAULT_PROMPT.to_owned(),
        DEFAULT_MAX_TOKENS,
        max_requests,
        DEFAULT_GATEWAY_ENDPOINT.to_owned(),
        token,
        tenant,
    )
}

fn parse_key_value_args(
    program: &str,
    args: &[String],
) -> (
    usize,
    u64,
    String,
    String,
    bool,
    String,
    u32,
    Option<usize>,
    String,
    Option<String>,
    String,
) {
    let mut concurrency: Option<usize> = None;
    let mut duration_secs: Option<u64> = None;
    let mut modelalias: Option<String> = None;
    let mut model: Option<String> = None;
    let mut show_outputs: Option<bool> = None;
    let mut prompt: Option<String> = None;
    let mut max_tokens: Option<u32> = None;
    let mut max_requests: Option<usize> = None;
    let mut gateway_endpoint: Option<String> = None;
    let mut token: Option<String> = None;
    let mut tenant: Option<String> = None;

    for arg in args {
        let (key, raw_value) = arg
            .split_once('=')
            .unwrap_or_else(|| fatal(program, &format!("invalid argument '{}', expected key=value", arg)));
        let key = key.trim().to_lowercase();
        let value = raw_value.trim();

        if key.is_empty() {
            fatal(program, "argument key cannot be empty");
        }
        if value.is_empty() {
            fatal(program, &format!("argument '{}' cannot be empty", key));
        }

        match key.as_str() {
            "concurrency" | "concurrent" | "concurrently" | "c" => {
                let parsed = value
                    .parse::<usize>()
                    .unwrap_or_else(|_| fatal(program, &format!("invalid concurrency '{}'", value)));
                concurrency = Some(parsed);
            }
            "duration" | "dur" | "durtaion" | "duration_secs" | "time" => {
                let secs = parse_duration_arg(value)
                    .unwrap_or_else(|msg| fatal(program, &msg));
                duration_secs = Some(secs);
            }
            "model" => {
                model = Some(value.to_owned());
            }
            "alias" | "modelalias" | "name" => {
                modelalias = Some(value.to_owned());
            }
            "show_output" | "show_outputs" | "show" | "dump_outputs" | "dump" => {
                let parsed = parse_bool_arg(value)
                    .unwrap_or_else(|msg| fatal(program, &msg));
                show_outputs = Some(parsed);
            }
            "prompt" => {
                prompt = Some(value.to_owned());
            }
            "max_tokens" | "maxtokens" | "tokens" => {
                let parsed = value
                    .parse::<u32>()
                    .unwrap_or_else(|_| fatal(program, &format!("invalid max_tokens '{}'", value)));
                if parsed == 0 {
                    fatal(program, "max_tokens must be greater than zero");
                }
                max_tokens = Some(parsed);
            }
            "max_requests" | "max_request" | "maxreq" | "requests" | "reqs" => {
                let parsed = parse_positive_usize(program, "max_requests", value);
                max_requests = Some(parsed);
            }
            "gatewayendpoint" | "gateway_endpoint" | "gateway" | "endpoint" => {
                gateway_endpoint = Some(value.to_owned());
            }
            "token" | "bearer" | "bearer_token" | "auth" | "authorization" => {
                token = Some(value.to_owned());
            }
            "tenant" => {
                tenant = Some(value.to_owned());
            }
            _ => fatal(program, &format!("unknown argument '{}'", key)),
        }
    }

    let model = model.unwrap_or_else(|| fatal(program, "model is required"));
    let concurrency = concurrency.unwrap_or_else(|| fatal(program, "concurrency is required"));
    let duration_secs = duration_secs.unwrap_or_else(|| fatal(program, "duration is required"));
    let modelalias = modelalias.unwrap_or_else(|| model.clone());
    let show_outputs = show_outputs.unwrap_or(false);
    let prompt = prompt.unwrap_or_else(|| DEFAULT_PROMPT.to_owned());
    let max_tokens = max_tokens.unwrap_or(DEFAULT_MAX_TOKENS);
    let max_requests = max_requests;
    let gateway_endpoint = gateway_endpoint
        .unwrap_or_else(|| DEFAULT_GATEWAY_ENDPOINT.to_owned());
    let tenant = tenant.unwrap_or_else(|| DEFAULT_TENANT.to_owned());

    (
        concurrency,
        duration_secs,
        modelalias,
        model,
        show_outputs,
        prompt,
        max_tokens,
        max_requests,
        gateway_endpoint,
        token,
        tenant,
    )
}

fn parse_duration_arg(value: &str) -> Result<u64, String> {
    let trimmed = value.trim().to_lowercase();
    if trimmed.is_empty() {
        return Err("duration cannot be empty".into());
    }

    let (number_part, multiplier) = if let Some(num) = trimmed.strip_suffix('s') {
        (num, 1)
    } else if let Some(num) = trimmed.strip_suffix('m') {
        (num, 60)
    } else if let Some(num) = trimmed.strip_suffix('h') {
        (num, 3600)
    } else {
        (trimmed.as_str(), 1)
    };

    let number = number_part
        .parse::<u64>()
        .map_err(|_| format!("invalid duration '{}'", value))?;
    number
        .checked_mul(multiplier)
        .ok_or_else(|| "duration value is too large".into())
}

fn print_usage(program: &str) {
    eprintln!(
        "Usage (positional): {} <concurrency> <duration_secs> [<modelalias>] <model> [<max_requests>] [<token>] [<tenant>]",
        program
    );
    eprintln!(
        "Usage (key-value): {} concurrency=<n> duration=<Ns|Nm|Nh> [alias=<modelalias>] model=<model> [prompt=<text>] [max_tokens=<n>] [max_requests=<n>] [gatewayendpoint=<url>] [token=<value>] [tenant=<name>] [show_output=<true|false>]",
        program
    );
    eprintln!(
        "Example: {} concurrency=10 duration=20s model=\"deepseek-ai/DeepSeek-R1-Distill-Llama-70B_8GPU\" prompt=\"Write a haiku\" max_tokens=64 max_requests=500 gatewayendpoint=\"http://localhost:31501\" token=\"YOUR_TOKEN\" tenant=\"public\"",
        program
    );
}

fn fatal(program: &str, msg: &str) -> ! {
    eprintln!("Error: {}", msg);
    print_usage(program);
    process::exit(1);
}

fn parse_bool_arg(value: &str) -> Result<bool, String> {
    match value.trim().to_lowercase().as_str() {
        "1" | "true" | "yes" | "y" | "on" | "enable" | "enabled" => Ok(true),
        "0" | "false" | "no" | "n" | "off" | "disable" | "disabled" => Ok(false),
        _ => Err(format!("invalid boolean value '{}'", value)),
    }
}

fn try_parse_positive_usize(value: &str) -> Option<usize> {
    value
        .parse::<usize>()
        .ok()
        .filter(|v| *v > 0)
}

fn parse_positive_usize(program: &str, field: &str, value: &str) -> usize {
    try_parse_positive_usize(value).unwrap_or_else(|| {
        fatal(
            program,
            &format!("invalid {} '{}', expected positive integer", field, value),
        )
    })
}
