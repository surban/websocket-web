use wasm_bindgen::JsValue;
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;
use web_sys::ReadableStreamDefaultReader;

use super::*;

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        web_sys::console::log_1(&JsValue::from_str(&format!($($arg)*)));
    }
}
pub use log;


#[wasm_bindgen_test]
async fn test_websocket_stream() {
    let url = "ws://127.0.0.1:8765";
    let mut builder = WebSocketBuilder::new(url);
    builder.set_interface(Interface::Stream);
    let ws = builder.connect().await.unwrap();

    log!("WebSocket connected: {ws:?}");
}