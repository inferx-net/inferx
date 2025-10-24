use std::{
    collections::BTreeMap, env, sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    }, time::{Duration, Instant}
};

use reqwest::Client;
use serde_json::json;
use serde_json::Value;
use tokio::time::sleep;

#[tokio::main]
async fn main() {
    // Usage: cargo run -- <concurrency> <duration_secs> <model>
    let args: Vec<String> = env::args().collect();
    if args.len() != 4 {
        eprintln!("Usage: {} <concurrency> <duration_secs> <model>", args[0]);
        std::process::exit(1);
    }

    let concurrency: usize = args[1].parse().expect("invalid concurrency");
    let duration_secs: u64 = args[2].parse().expect("invalid duration");
    let model = &args[3];

    run_hey(concurrency, duration_secs, model).await;
}

async fn run_hey(concurrency: usize, duration_secs: u64, model: &str) {
    let url = format!(
        // "http://localhost:4000/funccall/public/{}/v1/completions",
        "http://localhost:31501/funccall/public/{}/v1/completions",
        model
    );
    println!(
        "=== Running hey for model: {} (c={}, z={}s) ===",
        model, concurrency, duration_secs
    );

    let client = Client::new();
    let stop_time = Instant::now() + Duration::from_secs(duration_secs);

    let success = Arc::new(AtomicUsize::new(0));
    let fail = Arc::new(AtomicUsize::new(0));
    let mismatch = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();

    for _ in 0..concurrency {
        let client = client.clone();
        let url = url.clone();
        let success = success.clone();
        let fail = fail.clone();
        let mismatch = mismatch.clone();

        let model = model.to_string();
        handles.push(tokio::spawn(async move {
            let mut first = None;
            while Instant::now() < stop_time {
                let body = json!({
                    "prompt": "Can you provide ways to eat combinations of bananas and dragonfruits?",
                    "max_tokens": "10",
                    "model": model,
                    "stream": "false",
                    "temperature": "0",
                    "top_p": 1.0,
                    "seed": 0
                });

                let mut output = BTreeMap::new();

                match client.post(&url).json(&body).send().await {
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

                                if output.len() > 1 {
                                    println!("mismatch text {:?}", output);
                                }
                            }
                            Ok(_) => {
                                println!("status is {:?}", &status);
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
                sleep(Duration::from_millis(10)).await;
            }
        }));
    }

    for h in handles {
        let _ = h.await;
    }

    println!(
        "[200 OK & match]:\t{}\t[mismatch]:\t\t{}\t[fail]:\t\t\t{}\t\n",
        success.load(Ordering::Relaxed),
        mismatch.load(Ordering::Relaxed),
        fail.load(Ordering::Relaxed),
    );
}
