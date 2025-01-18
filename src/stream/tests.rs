use wasm_bindgen::JsValue;
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;
use web_sys::ReadableStreamDefaultReader;

use super::*;
use crate::tests::log;

#[wasm_bindgen_test]
async fn echo() {
    let target = "ws://127.0.0.1:8765";
    log!("connecting to {target}");
    let ws = WebSocketStream::new(target, &JsValue::null());

    log!("getting opened");
    let opened = ws.opened().await.unwrap();    
    log!("connected to protocol {}", opened.protocol());

    log!("getting reader writer");
    let writable = opened.writable();
    let writer = writable.get_writer().unwrap();
    let readable = opened.readable();
    let reader = readable.get_reader().dyn_into::<ReadableStreamDefaultReader>().unwrap();

    log!("sending packet");
    let promise = writer.write_with_chunk(&JsValue::from_str("packet"));
    JsFuture::from(promise).await.unwrap();

    log!("receiving packet");
    let promise = reader.read();
    let res = JsFuture::from(promise).await.unwrap();
    log!("receive result: {res:?}");

    log!("closing");
    ws.close(&JsValue::null());
}

#[wasm_bindgen_test]
async fn connect_fail() {
    let target = "ws://127.0.0.1:8764";
    log!("connecting to {target}");
    let ws = WebSocketStream::new(target, &JsValue::null());

    log!("getting opened");
    assert!(ws.opened().await.is_err());
    log!("failed correctly");
}
