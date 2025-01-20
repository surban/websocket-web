use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen_test]
async fn example() {
    use futures_util::{SinkExt, StreamExt};
    use websocket_web::{CloseCode, WebSocket};

    // Connect to WebSocket echo server running on localhost.
    let mut socket = WebSocket::connect("ws://127.0.0.1:8765").await.unwrap();

    // Send text message.
    socket.send("Test123").await.unwrap();

    // Receive message.
    let msg = socket.next().await.unwrap().unwrap();
    assert_eq!(msg.to_string(), "Test123");

    // Explicitly close WebSocket with close code and reason.
    socket.close_with_reason(CloseCode::NormalClosure, "Goodbye!");
}
