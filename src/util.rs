//! Utils.

use js_sys::{global, Promise, Reflect};
use std::{
    io::{self, ErrorKind},
    time::Duration,
};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Window, WorkerGlobalScope};

/// Sleep for the specified duration.
pub async fn sleep(duration: Duration) {
    let ms = duration.as_millis() as i32;
    let promise = Promise::new(&mut |resolve, _reject| {
        let global = global();
        if let Some(window) = global.dyn_ref::<Window>() {
            window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms).unwrap();
        } else if let Some(worker) = global.dyn_ref::<WorkerGlobalScope>() {
            worker.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms).unwrap();
        } else {
            panic!("unsupported global scope");
        }
    });
    JsFuture::from(promise).await.unwrap();
}

/// Extracts the error message from a JavaScript error.
pub fn js_err_msg(value: &JsValue) -> Option<String> {
    if let Some(js_err) = value.dyn_ref::<js_sys::Error>() {
        return js_err.message().as_string();
    }

    if let Some(event) = value.dyn_ref::<web_sys::Event>() {
        return Some(event.type_());
    }

    if Reflect::has(value, &JsValue::from_str("toString")).unwrap_or_default() {
        if let Ok(func) =
            Reflect::get(value, &JsValue::from_str("toString")).and_then(|v| v.dyn_into::<js_sys::Function>())
        {
            if let Ok(res) = func.call0(value) {
                return res.as_string();
            }
        }
    }

    None
}

/// Creates an IO error from the JavaScript error.
pub fn js_err(kind: ErrorKind, value: &JsValue) -> io::Error {
    let msg = js_err_msg(value).unwrap_or_else(|| kind.to_string());
    io::Error::new(kind, msg)
}
