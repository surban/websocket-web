//! WebSocket server for testing websocket-web.

use std::io::Error;

use futures::{future, SinkExt, StreamExt, TryStreamExt};
use log::{info, warn};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::Message;

#[tokio::main]
async fn main() -> Result<(), Error> {
    env_logger::Builder::new().filter(None, log::LevelFilter::Debug).init();

    // Create the event loop and TCP listener we'll accept connections on.
    let addr = "127.0.0.1:8765";
    let try_socket = TcpListener::bind(addr).await;
    let listener = try_socket.expect("Failed to bind");
    info!("Listening on: {}", addr);

    while let Ok((stream, _)) = listener.accept().await {
        tokio::spawn(accept_connection(stream));
    }

    Ok(())
}

async fn accept_connection(stream: TcpStream) {
    let addr = stream.peer_addr().expect("connected streams should have a peer address");
    info!("Peer address: {}", addr);

    let ws_stream =
        tokio_tungstenite::accept_async(stream).await.expect("Error during the websocket handshake occurred");

    info!("New WebSocket connection: {}", addr);

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
