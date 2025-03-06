//! WebSocket server for testing websocket-web.

use futures::{future, SinkExt, StreamExt, TryStreamExt};
use log::{info, warn};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{oneshot, oneshot::error::TryRecvError},
    task::JoinSet,
    time::Instant,
    try_join,
};
use tokio_tungstenite::tungstenite::{
    protocol::{frame::coding::CloseCode, CloseFrame},
    Bytes, Message,
};

#[tokio::main]
async fn main() {
    env_logger::Builder::new().filter(None, log::LevelFilter::Debug).init();

    let echo_server = tokio::spawn(echo_server());
    let speed_server = tokio::spawn(speed_server());

    try_join!(echo_server, speed_server).unwrap();
}

async fn echo_server() {
    let addr = "127.0.0.1:8765";
    let try_socket = TcpListener::bind(addr).await;
    let listener = try_socket.expect("Failed to bind");
    info!("Echo listening on: {}", addr);

    while let Ok((stream, _)) = listener.accept().await {
        tokio::spawn(accept_echo(stream));
    }
}

async fn accept_echo(stream: TcpStream) {
    let addr = stream.peer_addr().expect("connected streams should have a peer address");
    let ws_stream =
        tokio_tungstenite::accept_async(stream).await.expect("Error during the websocket handshake occurred");
    info!("New WebSocket echo connection: {}", addr);

    let (mut write, mut read) = ws_stream.split();

    while let Some(msg) = read.next().await {
        let msg = match msg {
            Ok(msg) => msg,
            Err(err) => {
                warn!("Error receiving message: {err}");
                return;
            }
        };

        let res = match msg {
            Message::Text(utf8_bytes) => {
                if utf8_bytes.to_string().starts_with("CLOSE") {
                    write
                        .send(Message::Close(Some(CloseFrame {
                            code: CloseCode::Library(3999),
                            reason: utf8_bytes,
                        })))
                        .await
                } else {
                    write.send(Message::Text(utf8_bytes)).await
                }
            }
            Message::Binary(bytes) => write.send(Message::Binary(bytes)).await,
            _ => continue,
        };
        if let Err(err) = res {
            warn!("Error sending message: {err}");
            return;
        }
    }

    read.try_filter(|msg| future::ready(msg.is_text() || msg.is_binary()))
        .forward(write)
        .await
        .expect("Failed to forward messages")
}

async fn speed_server() {
    let addr = "127.0.0.1:8766";
    let try_socket = TcpListener::bind(addr).await;
    let listener = try_socket.expect("Failed to bind");
    info!("Speed listening on: {}", addr);

    while let Ok((stream, _)) = listener.accept().await {
        tokio::spawn(accept_speed(stream));
    }
}

async fn accept_speed(stream: TcpStream) {
    const MSG_SIZE: usize = 4096;
    const MB: usize = 1_048_576;

    let addr = stream.peer_addr().expect("connected streams should have a peer address");
    let ws_stream =
        tokio_tungstenite::accept_async(stream).await.expect("Error during the websocket handshake occurred");
    info!("New WebSocket speed connection: {}", addr);

    let mut js = JoinSet::new();
    let (mut write, mut read) = ws_stream.split();

    let Some(Ok(Message::Text(mode))) = read.next().await else { return };
    info!("Speed test mode is {mode}");

    let (closed_tx, mut closed_rx) = oneshot::channel();

    // Sender
    if mode == "recv" || mode == "both" {
        js.spawn(async move {
            let start = Instant::now();
            let mut total = 0;

            let msg = Bytes::from(vec![1; MSG_SIZE]);
            while closed_rx.try_recv() == Err(TryRecvError::Empty) {
                if write.feed(Message::Binary(msg.clone())).await.is_err() {
                    break;
                }
                total += msg.len();
            }

            let mb = total as f64 / MB as f64;
            let secs = start.elapsed().as_secs_f64();
            println!("Sent {mb} MB in {secs:.1} seconds => {} MB/s", mb / secs);
        });
    }

    // Receiver
    if mode == "send" || mode == "both" {
        js.spawn(async move {
            let start = Instant::now();
            let mut total = 0;

            while let Some(Ok(msg)) = read.next().await {
                if let Message::Binary(data) = msg {
                    total += data.len();
                } else if let Message::Close(_) = msg {
                    break;
                }
            }

            let mb = total as f64 / MB as f64;
            let secs = start.elapsed().as_secs_f64();
            println!("Received {mb} MB in {secs:.1} seconds => {} MB/s", mb / secs);

            let _ = closed_tx.send(());
        });
    } else {
        js.spawn(async move {
            while let Some(Ok(msg)) = read.next().await {
                if let Message::Close(_) = msg {
                    break;
                }
            }

            let _ = closed_tx.send(());
        });
    }

    js.join_all().await;
}
