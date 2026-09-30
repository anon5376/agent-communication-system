//! `qagent-openai-compatible` — the generic OpenAI-compatible harness bin,
//! a verbatim port of `src/openai-compatible-harness.ts`. Posts one
//! chat/completions request and prints a normalized JSON result on stdout.

use serde_json::{json, Value};
use std::time::Duration;

const USAGE: &str = "qagent OpenAI-compatible harness\n\n--base-url URL --model MODEL --prompt TEXT [--api-key-env NAME]\n";

fn argument(argv: &[String], name: &str) -> Option<String> {
    let index = argv.iter().position(|a| a == name)?;
    Some(argv.get(index + 1).cloned().unwrap_or_default())
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn main_inner() -> Result<i32, String> {
    let argv: Vec<String> = std::env::args().collect();
    if argv.iter().any(|a| a == "--help" || a == "-h") {
        print!("{USAGE}");
        return Ok(0);
    }

    let base_url =
        argument(&argv, "--base-url").unwrap_or_else(|| "http://127.0.0.1:1234/v1".to_string());
    let base_url = base_url.trim_end_matches('/').to_string();
    let model = argument(&argv, "--model").unwrap_or_default();
    let prompt = argument(&argv, "--prompt").unwrap_or_default();
    if model.is_empty() {
        return Err("--model is required".to_string());
    }
    if prompt.is_empty() {
        return Err("--prompt is required".to_string());
    }
    let api_key_env =
        argument(&argv, "--api-key-env").unwrap_or_else(|| "OPENAI_API_KEY".to_string());
    let temperature = argument(&argv, "--temperature").and_then(|v| v.parse::<f64>().ok());
    let max_tokens = argument(&argv, "--max-tokens").and_then(|v| v.parse::<u64>().ok());

    let timeout_ms: u64 = env("QAGENT_HTTP_TIMEOUT_MS")
        .or_else(|| env("AGENT_BUS_HTTP_TIMEOUT_MS"))
        .and_then(|v| v.parse().ok())
        .unwrap_or(3_600_000);

    let api_key = env(&api_key_env).unwrap_or_default();
    let mut body = json!({
        "model": model,
        "messages": [{ "role": "user", "content": prompt }],
        "stream": false,
    });
    if let Some(t) = temperature {
        body["temperature"] = json!(t);
    }
    if let Some(m) = max_tokens {
        body["max_tokens"] = json!(m);
    }

    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_millis(timeout_ms)))
        .build()
        .new_agent();
    let mut request = agent
        .post(format!("{base_url}/chat/completions"))
        .header("content-type", "application/json");
    if !api_key.is_empty() {
        request = request.header("authorization", &format!("Bearer {api_key}"));
    }
    let mut response = request.send_json(&body).map_err(|e| format!("{e}"))?;
    let status = response.status().as_u16();
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("{e}"))?;
    if !(200..300).contains(&status) {
        return Err(format!(
            "OpenAI-compatible endpoint returned {status}: {}",
            text.chars().take(1000).collect::<String>()
        ));
    }
    let payload: Value =
        serde_json::from_str(&text).map_err(|e| format!("invalid JSON response: {e}"))?;
    let content = payload["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| {
            "OpenAI-compatible response did not contain choices[0].message.content".to_string()
        })?;
    let usage = &payload["usage"];
    let out = json!({
        "result": content,
        "usage": {
            "inputTokens": usage["prompt_tokens"].as_f64().unwrap_or(0.0),
            "outputTokens": usage["completion_tokens"].as_f64().unwrap_or(0.0),
            "totalTokens": usage["total_tokens"].as_f64().unwrap_or(0.0),
            "costUSD": 0,
        },
        "model": payload["model"].as_str().unwrap_or(&model),
        "id": payload["id"],
    });
    println!("{}", serde_json::to_string(&out).unwrap_or_default());
    Ok(0)
}

pub fn main() -> i32 {
    match main_inner() {
        Ok(code) => code,
        Err(message) => {
            eprintln!("{message}");
            1
        }
    }
}
