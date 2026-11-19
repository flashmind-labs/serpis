use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;
use serpis_ai::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const OK: &str = "data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\ndata: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
const RATE_LIMITED_IN_STREAM: &str = ": OPENROUTER PROCESSING\n\ndata: {\"error\":{\"code\":429,\"message\":\"Provider returned error\",\"metadata\":{\"raw\":\"temporarily rate-limited upstream\"}}}\n\n";
const FAILS_AFTER_TEXT: &str = "data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\ndata: {\"error\":{\"code\":502,\"message\":\"upstream died\"}}\n\n";

fn sse(body: &str) -> String {
    format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
}

fn status(code: u16, body: &str) -> String {
    format!("HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nRetry-After: 0\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
}

async fn serve(replies: Vec<String>) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            let n = seen.fetch_add(1, Ordering::SeqCst);
            let reply = replies[n.min(replies.len() - 1)].clone();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            while let Ok(k) = sock.read(&mut chunk).await {
                if k == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..k]);
                let text = String::from_utf8_lossy(&buf);
                if let Some(end) = text.find("\r\n\r\n") {
                    let len = text[..end].lines().find_map(|l| l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap())).unwrap_or(0);
                    if buf.len() >= end + 4 + len {
                        break;
                    }
                }
            }
            let _ = sock.write_all(reply.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });
    (url, count)
}

async fn ask(replies: Vec<String>, max_retries: Option<u32>) -> (AssistantMessage, usize) {
    let (url, count) = serve(replies).await;
    let model: Model = serde_json::from_value(json!({"id": "m", "api": "openai-completions", "provider": "openrouter", "baseUrl": url, "reasoning": false, "input": ["text"]})).unwrap();
    let ctx = Context { system_prompt: String::new(), messages: vec![Message::User { content: vec![UserContent::Text { text: "hi".into() }], timestamp: 1 }], tools: vec![] };
    let opts = StreamOptions { api_key: Some("k".into()), max_retries, ..Default::default() };
    let mut rx = stream(&model, &ctx, opts);
    let mut starts = 0;
    while let Some(ev) = rx.recv().await {
        match ev {
            StreamEvent::Start => starts += 1,
            StreamEvent::Done(m) | StreamEvent::Error(m) => {
                assert!(starts <= 1, "Start is sent at most once however many tries it took");
                return (m, count.load(Ordering::SeqCst));
            }
            _ => {}
        }
    }
    panic!("stream ended without Done or Error");
}

fn text(m: &AssistantMessage) -> String {
    m.content.iter().filter_map(|c| if let AssistantContent::Text { text, .. } = c { Some(text.as_str()) } else { None }).collect()
}

#[tokio::test]
async fn an_http_rate_limit_is_retried() {
    let (m, requests) = ask(vec![status(429, r#"{"error":{"message":"Rate limit exceeded"}}"#), sse(OK)], None).await;
    assert_eq!((m.stop_reason, text(&m), requests), (StopReason::Stop, "hi".into(), 2), "{:?}", m.error_message);
}

#[tokio::test]
async fn a_rate_limit_inside_the_stream_is_retried() {
    let (m, requests) = ask(vec![sse(RATE_LIMITED_IN_STREAM), sse(OK)], None).await;
    assert_eq!((m.stop_reason, text(&m), requests), (StopReason::Stop, "hi".into(), 2), "{:?}", m.error_message);
}

#[tokio::test]
async fn retries_stop_at_the_limit_and_say_so() {
    let (m, requests) = ask(vec![status(429, r#"{"error":{"message":"Rate limit exceeded"}}"#)], Some(2)).await;
    assert_eq!((m.stop_reason, requests), (StopReason::Error, 3));
    let error = m.error_message.unwrap();
    assert!(error.contains("gave up after 2 retries") && error.contains("429 Rate limit exceeded"), "{error}");
}

#[tokio::test]
async fn a_request_the_provider_rejects_is_not_retried() {
    let (m, requests) = ask(vec![status(400, r#"{"error":{"message":"bad request"}}"#), sse(OK)], None).await;
    assert_eq!((m.stop_reason, requests), (StopReason::Error, 1));
}

#[tokio::test]
async fn a_failure_after_text_streamed_is_not_retried() {
    let (m, requests) = ask(vec![sse(FAILS_AFTER_TEXT), sse(OK)], None).await;
    assert_eq!((m.stop_reason, text(&m), requests), (StopReason::Error, "partial".into(), 1));
}
