//! Outbound WebSocket publisher: bitstream plus JSON wire (PIN hash, flags, inbound control).

use crate::config::Config;
use crate::hub::Hub;
use crate::protocol::{pack_media, TYPE_FRAG, TYPE_INIT};
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::{broadcast, mpsc};
use tokio::time::sleep;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

const SEND_TIMEOUT: Duration = Duration::from_secs(2);
const PING_EVERY: Duration = Duration::from_secs(10);
const PUBLISH_QUEUE: usize = 32;

/// Timeouts are transient backpressure from the Durable Object. Reconnect
/// on those used to stall the watch page for ~30s at a time.
pub fn skip_on_send_timeout(err: &anyhow::Error) -> bool {
    let s = err.to_string();
    s.contains("publisher send timeout")
        || s.contains("publisher text timeout")
        || s.contains("publisher ping timeout")
}

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn send_bin(ws: &mut Ws, payload: Vec<u8>) -> anyhow::Result<()> {
    tokio::time::timeout(SEND_TIMEOUT, ws.send(Message::Binary(payload.into())))
        .await
        .map_err(|_| anyhow::anyhow!("publisher send timeout"))??;
    Ok(())
}

async fn send_text(ws: &mut Ws, payload: String) -> anyhow::Result<()> {
    tokio::time::timeout(SEND_TIMEOUT, ws.send(Message::Text(payload.into())))
        .await
        .map_err(|_| anyhow::anyhow!("publisher text timeout"))??;
    Ok(())
}

async fn send_ping(ws: &mut Ws, msg: Message) -> anyhow::Result<()> {
    tokio::time::timeout(SEND_TIMEOUT, ws.send(msg))
        .await
        .map_err(|_| anyhow::anyhow!("publisher ping timeout"))??;
    Ok(())
}

pub fn next_backoff(prev: Duration) -> Duration {
    let doubled = prev.saturating_mul(2);
    doubled.clamp(Duration::from_secs(1), Duration::from_secs(30))
}

pub fn publish_url_with_token(base: &str, token: &str) -> anyhow::Result<url::Url> {
    let mut u = url::Url::parse(base)?;
    if !token.is_empty() && u.query_pairs().all(|(k, _)| k != "token") {
        u.query_pairs_mut().append_pair("token", token);
    }
    Ok(u)
}

#[derive(Clone)]
pub struct Publisher {
    hub: Hub,
    cfg: Arc<Mutex<Config>>,
    task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    wire_out: broadcast::Sender<String>,
    latest_wire: Arc<Mutex<Vec<String>>>,
    inbound: mpsc::Sender<String>,
}

impl Publisher {
    pub fn new(hub: Hub, cfg: Config, inbound: mpsc::Sender<String>) -> Self {
        let (wire_out, _) = broadcast::channel(2048);
        Self {
            hub,
            cfg: Arc::new(Mutex::new(cfg)),
            task: Arc::new(Mutex::new(None)),
            wire_out,
            latest_wire: Arc::new(Mutex::new(Vec::new())),
            inbound,
        }
    }

    pub fn set_config(&self, cfg: Config) {
        *self.cfg.lock() = cfg;
    }

    pub fn push_wire(&self, msg: String) {
        {
            let mut latest = self.latest_wire.lock();
            // Keep last otp + last flags (replace same type).
            if msg.contains("\"otp\"") {
                latest.retain(|m| !m.contains("\"otp\""));
            }
            if msg.contains("\"flags\"") {
                latest.retain(|m| !m.contains("\"flags\""));
            }
            if msg.contains("\"clipboard\"") {
                latest.retain(|m| !m.contains("\"clipboard\""));
            }
            if msg.contains("\"thumbs\"") {
                latest.retain(|m| !m.contains("\"thumbs\""));
            }
            let chunked_clip = msg.contains("\"clipboard\"")
                && (msg.contains("\"action\":\"begin\"")
                    || msg.contains("\"action\":\"chunk\"")
                    || msg.contains("\"action\":\"end\""));
            if msg.contains("\"type\":\"file\"") || chunked_clip {
                // File blobs and chunked clipboard PNGs are transient.
            } else {
                latest.push(msg.clone());
            }
        }
        let _ = self.wire_out.send(msg);
    }

    pub fn start(&self) {
        self.stop();
        let hub = self.hub.clone();
        let cfg = self.cfg.clone();
        let wire_out = self.wire_out.clone();
        let latest_wire = self.latest_wire.clone();
        let inbound = self.inbound.clone();
        let handle = tokio::spawn(async move {
            let mut backoff = Duration::from_secs(1);
            loop {
                let (url, token) = {
                    let c = cfg.lock();
                    (c.cloudflare.publish_url.clone(), c.token.clone())
                };
                if url.is_empty() {
                    sleep(Duration::from_secs(1)).await;
                    continue;
                }
                match run_session(&hub, &url, &token, &wire_out, &latest_wire, &inbound).await {
                    Ok(()) => backoff = Duration::from_secs(1),
                    Err(e) => {
                        tracing::warn!("cloudflare publisher: {e}");
                        sleep(backoff).await;
                        backoff = next_backoff(backoff);
                    }
                }
            }
        });
        *self.task.lock() = Some(handle);
    }

    pub fn stop(&self) {
        if let Some(h) = self.task.lock().take() {
            h.abort();
        }
    }
}

async fn run_session(
    hub: &Hub,
    url: &str,
    token: &str,
    wire_out: &broadcast::Sender<String>,
    latest_wire: &Arc<Mutex<Vec<String>>>,
    inbound: &mpsc::Sender<String>,
) -> anyhow::Result<()> {
    let u = publish_url_with_token(url, token)?;
    tracing::info!("publishing to {}{}", u.host_str().unwrap_or("?"), u.path());
    let (mut ws, _) = tokio_tungstenite::connect_async(u.as_str()).await?;
    let sub = hub.subscribe(PUBLISH_QUEUE);
    if let Some(init) = hub.init_segment() {
        send_bin(&mut ws, pack_media(TYPE_INIT, &init)).await?;
    }
    if let Some(lat) = hub.latest() {
        if lat.kind == TYPE_FRAG {
            send_bin(&mut ws, pack_media(TYPE_FRAG, &lat.data)).await?;
        }
    }
    let pending = latest_wire.lock().clone();
    for msg in pending {
        send_text(&mut ws, msg).await?;
    }
    let mut json_rx = wire_out.subscribe();
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut frag_skips = 0u32;
    loop {
        tokio::select! {
            media = sub.recv() => {
                let Some(m) = media else {
                    anyhow::bail!("publisher hub closed");
                };
                if let Err(e) = send_bin(&mut ws, pack_media(m.kind, &m.data)).await {
                    if m.kind == TYPE_INIT || !skip_on_send_timeout(&e) {
                        return Err(e);
                    }
                    frag_skips = frag_skips.saturating_add(1);
                    if frag_skips >= 8 {
                        return Err(e);
                    }
                    tracing::warn!("cloudflare publisher: skip fragment ({e})");
                    continue;
                }
                frag_skips = 0;
            }
            json = json_rx.recv() => {
                if let Ok(msg) = json {
                    let auth_wire = msg.contains("\"otp\"") || msg.contains("\"flags\"");
                    if let Err(e) = send_text(&mut ws, msg).await {
                        if auth_wire || !skip_on_send_timeout(&e) {
                            return Err(e);
                        }
                        tracing::warn!("cloudflare publisher: skip json ({e})");
                    }
                }
            }
            _ = ping.tick() => {
                send_ping(&mut ws, Message::Ping(bytes::Bytes::new())).await?;
            }
            incoming = ws.next() => {
                match incoming {
                    None => anyhow::bail!("publisher websocket closed"),
                    Some(Err(e)) => return Err(e.into()),
                    Some(Ok(Message::Close(_))) => anyhow::bail!("publisher websocket close"),
                    Some(Ok(Message::Ping(p))) => {
                        send_ping(&mut ws, Message::Pong(p)).await?;
                    }
                    Some(Ok(Message::Text(t))) => {
                        if inbound.send(t.to_string()).await.is_err() {
                            anyhow::bail!("publisher inbound closed");
                        }
                    }
                    Some(Ok(Message::Binary(b))) => {
                        if let Ok(s) = std::str::from_utf8(&b) {
                            if s.starts_with('{') && inbound.send(s.to_string()).await.is_err() {
                                anyhow::bail!("publisher inbound closed");
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_caps_at_30s() {
        let mut d = Duration::from_secs(1);
        d = next_backoff(d);
        assert_eq!(d, Duration::from_secs(2));
        for _ in 0..10 {
            d = next_backoff(d);
        }
        assert_eq!(d, Duration::from_secs(30));
    }

    #[test]
    fn appends_token_query() {
        let u = publish_url_with_token("wss://example.com/publish", "secret").unwrap();
        assert!(u.as_str().contains("token=secret"));
        let u = publish_url_with_token("wss://example.com/publish?token=secret", "other").unwrap();
        assert_eq!(u.query_pairs().filter(|(k, _)| k == "token").count(), 1);
    }

    #[test]
    fn send_timeout_does_not_drop_session() {
        assert!(
            skip_on_send_timeout(&anyhow::anyhow!("publisher send timeout")),
            "a slow Durable Object ACK must skip a fragment, not reconnect the publisher"
        );
        assert!(!skip_on_send_timeout(&anyhow::anyhow!(
            "publisher websocket closed"
        )));
    }
}
