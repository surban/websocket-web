use futures_util::{SinkExt, StreamExt};
use tokio::sync::Semaphore;
use wasm_bindgen::JsValue;
use wasm_bindgen_test::wasm_bindgen_test;

use websocket_web::*;

mod util;
use util::{now, ResultExt};

fn echo_url() -> String {
    let host = web_sys::window().unwrap().location().hostname().unwrap();
    format!("ws://{host}:8765")
}

fn speed_url() -> String {
    let host = web_sys::window().unwrap().location().hostname().unwrap();
    format!("ws://{host}:8766")
}

/// Benchmark: echo roundtrip of small messages.
/// Measures per-message latency including Promise overhead.
async fn echo_small(interface: Interface) {
    const MSG: &str = "ping";
    const DURATION: f64 = 5.;

    static SEMAPHORE: Semaphore = Semaphore::const_new(1);
    let _permit = SEMAPHORE.acquire().await.unwrap_log();

    let url = echo_url();
    let mut builder = WebSocketBuilder::new(&url);
    builder.set_interface(interface);

    let mut socket = builder.connect().await.expect_log("connect failed");
    log!("echo_small: connected via {interface:?}");

    // Warm up.
    for _ in 0..10 {
        socket.send(MSG).await.unwrap_log();
        let _ = socket.next().await.unwrap_log().unwrap_log();
    }

    let start = now();
    let mut count: u64 = 0;

    while now() - start < DURATION {
        socket.send(MSG).await.unwrap_log();
        let _ = socket.next().await.unwrap_log().unwrap_log();
        count += 1;
    }

    let elapsed = now() - start;
    let msg_per_sec = count as f64 / elapsed;
    let avg_us = elapsed / count as f64 * 1_000_000.;
    msg!("echo_small {interface:?}: {count} roundtrips in {elapsed:.1}s => {msg_per_sec:.0} msg/s, avg {avg_us:.0} us/roundtrip");

    socket.close_with_reason(CloseCode::NormalClosure, "done");
}

/// Benchmark: send-only small messages (fire and measure throughput).
async fn send_small(interface: Interface) {
    const DURATION: f64 = 5.;

    static SEMAPHORE: Semaphore = Semaphore::const_new(1);
    let _permit = SEMAPHORE.acquire().await.unwrap_log();

    let url = speed_url();
    let mut builder = WebSocketBuilder::new(&url);
    builder.set_interface(interface);

    let mut socket = builder.connect().await.expect_log("connect failed");
    log!("send_small: connected via {:?}", socket.interface());

    // Tell speed server we're in "send" mode.
    socket.send("send").await.unwrap_log();

    let data: &[u8] = &[42u8; 10];
    let start = now();
    let mut count: u64 = 0;

    while now() - start < DURATION {
        socket.send(data).await.unwrap_log();
        count += 1;
    }

    let elapsed = now() - start;
    let msg_per_sec = count as f64 / elapsed;
    msg!("send_small {interface:?}: {count} msgs in {elapsed:.1}s => {msg_per_sec:.0} msg/s");

    socket.close_with_reason(CloseCode::NormalClosure, "done");
}

/// Benchmark: receive small messages from echo (batch send then batch receive).
async fn recv_small(interface: Interface) {
    const BATCH: usize = 1000;
    const DURATION: f64 = 5.;

    static SEMAPHORE: Semaphore = Semaphore::const_new(1);
    let _permit = SEMAPHORE.acquire().await.unwrap_log();

    let url = echo_url();
    let mut builder = WebSocketBuilder::new(&url);
    builder.set_interface(interface);

    let mut socket = builder.connect().await.expect_log("connect failed");
    log!("recv_small: connected via {interface:?}");

    let msg = "x";
    let start = now();
    let mut count: u64 = 0;

    while now() - start < DURATION {
        // Send a batch.
        for _ in 0..BATCH {
            socket.send(msg).await.unwrap_log();
        }
        // Receive the batch.
        for _ in 0..BATCH {
            let _ = socket.next().await.unwrap_log().unwrap_log();
        }
        count += BATCH as u64;
    }

    let elapsed = now() - start;
    let msg_per_sec = count as f64 / elapsed;
    msg!("recv_small {interface:?}: {count} msgs in {elapsed:.1}s => {msg_per_sec:.0} msg/s");

    socket.close_with_reason(CloseCode::NormalClosure, "done");
}

macro_rules! require_stream_support {
    () => {
        if !Interface::Stream.is_supported() {
            msg!("WebSocketStream not supported");
            return;
        }
    };
}

// --- Echo roundtrip benchmarks ---

#[wasm_bindgen_test]
async fn echo_small_stream() {
    require_stream_support!();
    echo_small(Interface::Stream).await;
}

#[wasm_bindgen_test]
async fn echo_small_standard() {
    echo_small(Interface::Standard).await;
}

// --- Send-only benchmarks ---

#[wasm_bindgen_test]
async fn send_small_stream() {
    require_stream_support!();
    send_small(Interface::Stream).await;
}

#[wasm_bindgen_test]
async fn send_small_standard() {
    send_small(Interface::Standard).await;
}

// --- Receive benchmarks (batched echo) ---

#[wasm_bindgen_test]
async fn recv_small_stream() {
    require_stream_support!();
    recv_small(Interface::Stream).await;
}

#[wasm_bindgen_test]
async fn recv_small_standard() {
    recv_small(Interface::Standard).await;
}
