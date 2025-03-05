use futures_util::{SinkExt, StreamExt};
use tokio::{
    io::{duplex, AsyncReadExt, AsyncWriteExt},
    sync::{mpsc, oneshot},
};
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::spawn_local;
use wasm_bindgen_test::wasm_bindgen_test;

use websocket_web::*;

mod util;
use util::ResultExt;

fn url() -> String {
    let host = web_sys::window().unwrap().location().hostname().unwrap();
    format!("ws://{host}:8765")
}

async fn echo(interface: Option<Interface>) {
    let url = url();
    let mut builder = WebSocketBuilder::new(&url);
    if let Some(interface) = interface {
        builder.set_interface(interface);
    }

    log!("Connecting to {url} using {interface:?}");
    let mut socket = builder.connect().await.expect_log("connect failed");
    log!("Connected: {socket:?}");

    let msg = "hi123";
    log!("Sending text message: {msg}");
    socket.send(msg).await.unwrap_log();

    log!("Receiving message");
    let recved = socket.next().await.unwrap_log().unwrap_log();
    log!("Received: {recved:?}");
    assert_eq!(recved, Msg::Text(msg.to_string()));

    let msg2 = [1, 2, 3, 11, 12, 13];
    log!("Sending binary message: {msg2:?}");
    socket.send(&msg2[..]).await.unwrap_log();

    log!("Receiving message");
    let recved2 = socket.next().await.unwrap_log().unwrap_log();
    log!("Received: {recved2:?}");
    assert_eq!(recved2, Msg::Binary(msg2.to_vec()));

    socket.close_with_reason(CloseCode::NormalClosure, "goodbye");
}

#[wasm_bindgen_test]
async fn echo_auto() {
    echo(None).await;
}

#[wasm_bindgen_test]
async fn echo_stream() {
    if !Interface::Stream.is_supported() {
        log!("WebSocketStream not supported");
        return;
    }
    echo(Some(Interface::Stream)).await;
}

#[wasm_bindgen_test]
async fn echo_standard() {
    echo(Some(Interface::Standard)).await;
}

async fn backpressure(interface: Option<Interface>) {
    const CNT: usize = 100_000;
    const CLOSE_MSG: &str = "CLOSE-123";

    let url = url();
    let mut builder = WebSocketBuilder::new(&url);
    if let Some(interface) = interface {
        builder.set_interface(interface);
    }

    log!("Connecting to {url} using {interface:?}");
    let socket = builder.connect().await.expect_log("connect failed");
    log!("Connected: {socket:?}");

    let closed = socket.closed();
    let (mut tx, mut rx) = socket.into_split();
    let (local_tx, mut local_rx) = mpsc::unbounded_channel();
    let (total_tx, total_rx) = oneshot::channel();

    spawn_local(async move {
        let mut total = 0;
        let mut rxed = 0;
        while let Some(msg) = rx.next().await {
            let msg = msg.expect_log("receive failed");
            let local_msg = local_rx.recv().await.unwrap_log();
            if msg != local_msg {
                panic_log!("recevied wrong message");
            }

            total += msg.len();
            rxed += 1;

            if rxed % 1000 == 0 {
                log!("Received message {rxed} / {CNT}");
            }
        }
        log!("Received {rxed} / {CNT} messages of total size {total} bytes");
        let None = local_rx.recv().await else { panic_log!("message missing") };

        log!("Waiting for close info");
        let reason = closed.await;
        log!("Close reason: {reason}");
        if reason.code != CloseCode::Other(3999) {
            panic_log!("Invalid close code {reason}");
        }
        if reason.reason != CLOSE_MSG {
            panic_log!("Invalid close reason: {reason}");
        }

        total_tx.send(total).unwrap_log();
    });

    for i in 1..=CNT {
        let len = i % 10_000;
        if i % 1000 == 0 {
            log!("Sending message {i} / {CNT} of size {len} bytes");
        }

        let data = vec![i as u8; len];
        tx.feed(data.clone()).await.expect_log("feed failed");
        local_tx.send(Msg::Binary(data)).unwrap_log();
    }
    log!("Flushing");
    <WebSocketSender as SinkExt<Vec<u8>>>::flush(&mut tx).await.expect_log("flush failed");

    log!("Sending {CLOSE_MSG}");
    tx.send(CLOSE_MSG).await.expect_log("send CLOSE failed");

    log!("Waiting for closure");
    drop(local_tx);
    let total = total_rx.await.expect_log("not okay");
    log!("Received {total} bytes");
}

#[wasm_bindgen_test]
async fn backpressure_stream() {
    if !Interface::Stream.is_supported() {
        log!("WebSocketStream not supported");
        return;
    }
    backpressure(Some(Interface::Stream)).await;
}

#[wasm_bindgen_test]
async fn backpressure_standard() {
    backpressure(Some(Interface::Standard)).await;
}

async fn io(interface: Option<Interface>) {
    const CNT: usize = 100_000;
    const CLOSE_MSG: &str = "CLOSE-123";

    let url = url();
    let mut builder = WebSocketBuilder::new(&url);
    if let Some(interface) = interface {
        builder.set_interface(interface);
    }

    log!("Connecting to {url} using {interface:?}");
    let socket = builder.connect().await.expect_log("connect failed");
    log!("Connected: {socket:?}");

    let (mut tx, mut rx) = socket.into_split();

    let (mut local_tx, mut local_rx) = duplex(100_000);
    let (total_tx, total_rx) = oneshot::channel();

    spawn_local(async move {
        let mut total = 0;
        loop {
            let mut buf = vec![0; 1024];
            let n = rx.read(&mut buf).await.expect_log("read failed");
            buf.truncate(n);
            if n == 0 {
                break;
            }

            let mut local_buf = vec![0; buf.len()];
            local_rx.read_exact(&mut local_buf).await.expect_log("local data ended");

            if buf != local_buf {
                panic_log!("recevied wrong message");
            }

            total += buf.len();
        }
        total_tx.send(total).unwrap_log();
    });

    let mut sent = 0;
    for i in 1..=CNT {
        let len = i % 10_000;
        if i % 1000 == 0 {
            log!("Sending message {i} / {CNT} of size {len} bytes");
        }

        let data = vec![i as u8; len];
        tx.write_all(&data).await.expect_log("write_all failed");
        local_tx.write_all(&data).await.unwrap_log();
        sent += data.len();
    }
    log!("Flushing");
    <WebSocketSender as AsyncWriteExt>::flush(&mut tx).await.expect_log("flush failed");
    local_tx.flush().await.unwrap_log();

    log!("Sending {CLOSE_MSG}");
    tx.send(CLOSE_MSG).await.expect_log("send CLOSE failed");

    log!("Waiting for closure");
    drop(local_tx);
    let total = total_rx.await.expect_log("not okay");
    log!("Received {total} bytes");
    assert_eq!(total, sent);
}

#[wasm_bindgen_test]
async fn io_stream() {
    if !Interface::Stream.is_supported() {
        msg!("WebSocketStream not supported");
        return;
    }
    io(Some(Interface::Stream)).await;
}

#[wasm_bindgen_test]
async fn io_standard() {
    io(Some(Interface::Standard)).await;
}
