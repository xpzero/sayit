// 云端插头：说 OpenAI 方言的任何服务（OpenAI / 硅基流动 / Groq / 本地 faster-whisper 服务器…）
use std::path::Path;
use std::time::Duration;

use crate::config::Config;

pub fn transcribe(cfg: &Config, wav: &Path) -> Result<String, String> {
    let cloud = &cfg.stt.cloud;
    if cloud.api_key.trim().is_empty() {
        return Err("未填写 API Key——去 设置 → 识别方式 → 云端识别 填写".into());
    }
    let base = cloud.endpoint.trim_end_matches('/');
    let url = format!("{base}/audio/transcriptions");

    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;

    let form = reqwest::blocking::multipart::Form::new()
        .file("file", wav)
        .map_err(|e| format!("读取音频失败：{e}"))?
        .text("model", cloud.model.clone())
        .text("language", cloud.language.clone());

    let resp = client
        .post(&url)
        .bearer_auth(cloud.api_key.trim())
        .timeout(Duration::from_secs(180))
        .multipart(form)
        .send()
        .map_err(|e| {
            if e.is_connect() {
                format!("连不上 {base}——检查网络或接口地址")
            } else {
                format!("请求失败：{e}")
            }
        })?;

    // 人话错误映射（第 3 讲的"失败三兄弟"）
    match resp.status().as_u16() {
        401 | 403 => return Err("密钥无效或过期——检查 API Key".into()),
        402 => return Err("服务方余额不足".into()),
        429 => return Err("请求太频繁或额度用尽（429）".into()),
        s if !(200..300).contains(&s) => {
            let body = resp.text().unwrap_or_default();
            let brief: String = body.chars().take(200).collect();
            return Err(format!("服务方返回 HTTP {s}：{brief}"));
        }
        _ => {}
    }

    let json: serde_json::Value = resp.json().map_err(|e| format!("响应解析失败：{e}"))?;
    // 标准 OpenAI 返回 { "text": "..." }；有些兼容服务包一层 error
    if let Some(err) = json["error"]["message"].as_str() {
        return Err(format!("服务方报错：{err}"));
    }
    Ok(json["text"].as_str().unwrap_or("").trim().to_string())
}
