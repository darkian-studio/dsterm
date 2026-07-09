#![allow(dead_code)]

use serde_json::Value;

fn base(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

/// GET a loopback endpoint and parse the JSON body.
pub async fn get_json(http: &reqwest::Client, port: u16, path: &str) -> anyhow::Result<Value> {
    let url = format!("{}{}", base(port), path);
    Ok(http.get(url).send().await?.json::<Value>().await?)
}

/// GET a loopback endpoint with query params and parse the JSON body.
pub async fn get_json_query(
    http: &reqwest::Client,
    port: u16,
    path: &str,
    params: &[(&str, String)],
) -> anyhow::Result<Value> {
    let url = format!("{}{}", base(port), path);
    Ok(http
        .get(url)
        .query(params)
        .send()
        .await?
        .json::<Value>()
        .await?)
}

/// POST a JSON body to a loopback endpoint and parse the JSON body.
pub async fn post_json(
    http: &reqwest::Client,
    port: u16,
    path: &str,
    body: &Value,
) -> anyhow::Result<Value> {
    let url = format!("{}{}", base(port), path);
    Ok(http
        .post(url)
        .json(body)
        .send()
        .await?
        .json::<Value>()
        .await?)
}

/// POST a JSON body to a loopback endpoint that replies with a plain-text body
/// (e.g. `/terminals` returns the PID as text).
pub async fn post_text(
    http: &reqwest::Client,
    port: u16,
    path: &str,
    body: &Value,
) -> anyhow::Result<String> {
    let url = format!("{}{}", base(port), path);
    Ok(http.post(url).json(body).send().await?.text().await?)
}
