use futures_util::{try_join, SinkExt, StreamExt};
use tokio::sync::{oneshot, Semaphore};
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::spawn_local;
use wasm_bindgen_test::wasm_bindgen_test;

use websocket_web::*;

mod util;
use util::{now, ResultExt};

fn url() -> String {
    let host = web_sys::window().unwrap().location().hostname().unwrap();
    format!("ws://{host}:8766")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Send,
    Recv,
    Both,
}

impl AsRef<str> for Mode {
    fn as_ref(&self) -> &'static str {
        match self {
            Mode::Send => "send",
            Mode::Recv => "recv",
            Mode::Both => "both",
        }
    }
}

async fn speed(interface: Option<Interface>, mode: Mode) {
    const MSG_SIZE: usize = 16_384;
    const MB: usize = 1_048_576;
    const DURATION: f64 = 10.;

    static SEMAPHORE: Semaphore = Semaphore::const_new(1);
    let _permit = SEMAPHORE.acquire().await.unwrap_log();

    let url = url();
    let mut builder = WebSocketBuilder::new(&url);
    if let Some(interface) = interface {
        builder.set_interface(interface);
    }

    log!("Connecting to {url} using {interface:?}");
    let mut socket = builder.connect().await.expect_log("connect failed");
    let interface = socket.interface();
    log!("Connected: {socket:?}");

    log!("Performing speed test in {mode:?} direction");
    socket.send(mode.as_ref()).await.unwrap_log();

    let (mut write, mut read) = socket.into_split();

    // Sender
    let (send_done_tx, send_done_rx) = oneshot::channel();
    if mode == Mode::Send || mode == Mode::Both {
        spawn_local(async move {
            let start = now();
            let mut total = 0;

            while now() - start < DURATION {
                let data = vec![2; MSG_SIZE];
                total += data.len();
                write.feed(data).await.unwrap_log();
            }

            let mb = total as f64 / MB as f64;
            let secs = now() - start;
            msg!(
                "Sent {mb} MB in {secs:.1} seconds using {interface:?} interface => {:.1} MB/s ({mode:?})",
                mb / secs
            );

            send_done_tx.send(()).unwrap();
        });
    } else {
        send_done_tx.send(()).unwrap();
    }

    // Receiver
    let (recv_done_tx, recv_done_rx) = oneshot::channel();
    if mode == Mode::Recv || mode == Mode::Both {
        spawn_local(async move {
            let start = now();
            let mut total = 0;

            while now() - start < DURATION {
                let msg = read.next().await.unwrap_log().unwrap_log();
                if let Msg::Binary(data) = msg {
                    total += data.len();
                }
            }

            let mb = total as f64 / MB as f64;
            let secs = now() - start;
            msg!(
                "Received {mb} MB in {secs:.1} seconds using {interface:?} interface => {:.1} MB/s ({mode:?})",
                mb / secs
            );

            recv_done_tx.send(()).unwrap();
        });
    } else {
        recv_done_tx.send(()).unwrap();
    }

    try_join!(send_done_rx, recv_done_rx).unwrap_log();
}

macro_rules! require_stream_support {
    () => {
        if !Interface::Stream.is_supported() {
            msg!("WebSocketStream not supported");
            return;
        }
    };
}

#[wasm_bindgen_test]
async fn send_stream() {
    require_stream_support!();
    speed(Some(Interface::Stream), Mode::Send).await;
}

#[wasm_bindgen_test]
async fn recv_stream() {
    require_stream_support!();
    speed(Some(Interface::Stream), Mode::Recv).await;
}

#[wasm_bindgen_test]
async fn both_stream() {
    require_stream_support!();
    speed(Some(Interface::Stream), Mode::Both).await;
}

#[wasm_bindgen_test]
async fn send_standard() {
    speed(Some(Interface::Standard), Mode::Send).await;
}

#[wasm_bindgen_test]
async fn recv_standard() {
    speed(Some(Interface::Standard), Mode::Recv).await;
}
