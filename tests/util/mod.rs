use std::fmt;

use wasm_bindgen::JsValue;

/// Log to console.
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        web_sys::console::log_1(&JsValue::from_str(&format!($($arg)*)));
    }
}

#[track_caller]
pub fn log_and_panic(msg: &str) -> ! {
    web_sys::console::error_1(&JsValue::from_str(msg));
    panic!("{msg}")
}

#[macro_export]
macro_rules! panic_log {
    ($($arg:tt)*) => {
        crate::util::log_and_panic(&format!($($arg)*))
    }
}

pub trait ResultExt<T> {
    #[track_caller]
    fn expect_log(self, msg: &str) -> T;
    #[track_caller]
    fn unwrap_log(self) -> T;
}

impl<T, E> ResultExt<T> for Result<T, E>
where
    E: fmt::Display,
{
    #[track_caller]
    fn expect_log(self, msg: &str) -> T {
        match self {
            Ok(v) => v,
            Err(err) => panic_log!("{msg}: {err}"),
        }
    }

    #[track_caller]
    fn unwrap_log(self) -> T {
        match self {
            Ok(v) => v,
            Err(err) => panic_log!("unwrap failed: {err}"),
        }
    }
}

impl<T> ResultExt<T> for Option<T> {
    #[track_caller]
    fn expect_log(self, msg: &str) -> T {
        match self {
            Some(v) => v,
            None => panic_log!("{msg}"),
        }
    }

    #[track_caller]
    fn unwrap_log(self) -> T {
        match self {
            Some(v) => v,
            None => panic_log!("unwrap of None option"),
        }
    }
}

/// Current time in seconds.
#[allow(dead_code)]
pub fn now() -> f64 {
    let window = web_sys::window().unwrap();
    let performance = window.performance().unwrap();
    performance.now() / 1000.
}

/// Message on page.
#[macro_export]
macro_rules! msg {
    ($($arg:tt)*) => {
        $crate::util::write_msg(&format!($($arg)*));
    }
}

#[allow(dead_code)]
pub fn write_msg(msg: &str) {
    log!("{msg}");

    let window = web_sys::window().unwrap();
    let document = window.document().unwrap();
    let body = document.body().unwrap();

    let message = document.create_element("div").unwrap();
    message.set_inner_html(&format!("<pre>{msg}</pre>"));
    body.append_child(&message).unwrap();
}
