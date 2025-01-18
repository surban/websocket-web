//! WebSocket

use futures::prelude::*;
use js_sys::{Array, Object, Promise, Reflect};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{ReadableStream, ReadableStreamDefaultReader, WritableStream};

use core::fmt;
use std::io;
use std::io::Error;
use std::io::ErrorKind;
use std::ops::Deref;
use std::rc::Rc;
use std::sync::Arc;

use crate::{Info, Interface, WebSocketBuilder};

struct StandardReaderInner {}

struct WebSocketGuard {
    socket: web_sys::WebSocket,
    closed: bool,
}

impl WebSocketGuard {
    fn new(socket: web_sys::WebSocket) -> Self {
        Self {
            socket,
            closed: false,
        }
    }
}

impl Deref for WebSocketGuard {
    type Target = web_sys::WebSocket;
    fn deref(&self) -> &Self::Target {
        &self.socket
    }
}

impl Drop for WebSocketGuard {
    fn drop(&mut self) {
        if !self.closed {
            let _ = self.socket.close();
        }
    }
}

pub struct ReaderInner {

}

pub struct Inner {
    socket: web_sys::WebSocket,
}

impl Inner {
    pub async fn new(builder: WebSocketBuilder) -> io::Result<(Self, Info)> {
        let protocols = Array::new();
        for proto in builder.protocols {
            protocols.push(&JsValue::from_str(&proto));
        }

        let socket =
            web_sys::WebSocket::new_with_str_sequence(&builder.url, &protocols).map_err(|err| {
                Error::new(ErrorKind::InvalidInput, err.as_string().unwrap_or_default())
            })?;

        let connect = Promise::new(&mut |resolve, reject| {
            socket.set_onopen(Some(&resolve));
            socket.set_onerror(Some(&reject));
        });
        JsFuture::from(connect).await.map_err(|err| {
            Error::new(
                ErrorKind::ConnectionRefused,
                err.as_string().unwrap_or_default(),
            )
        })?;

        let info = Info {
            url: builder.url,
            protocol: socket.protocol(),
            interface: Interface::Standard,
        };

        Ok((Self { socket }, info))
    }
}

struct StandardWriterInner {}
