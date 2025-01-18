//! WebSocketStream

use futures::prelude::*;
use js_sys::{Array, Object, Promise, Reflect, Uint8Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{ReadableStream, ReadableStreamDefaultReader, WritableStream, WritableStreamDefaultWriter};

use core::fmt;
use std::io;
use std::io::Error;
use std::io::ErrorKind;
use std::ops::Deref;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::task::{ready, Context, Poll};

use crate::closed::Closed;
use crate::{CloseCode, ClosedReason, Info, Interface, Msg, WebSocketBuilder};

#[cfg(test)]
mod tests;

#[wasm_bindgen]
extern "C" {
    type WebSocketStream;

    #[wasm_bindgen(constructor)]
    fn new(url: &str, options: &JsValue) -> WebSocketStream;

    #[wasm_bindgen(method, getter, catch)]
    async fn opened(this: &WebSocketStream) -> Result<WebSocketStreamOpened, JsValue>;

    #[wasm_bindgen(method, getter)]
    fn closed(this: &WebSocketStream) -> Promise;

    #[wasm_bindgen(method)]
    fn close(this: &WebSocketStream, options: &JsValue);

    #[wasm_bindgen(method, getter)]
    fn url(this: &WebSocketStream) -> String;
}

#[wasm_bindgen]
extern "C" {
    type WebSocketStreamOpened;

    #[wasm_bindgen(method, getter)]
    fn extensions(this: &WebSocketStreamOpened) -> String;

    #[wasm_bindgen(method, getter)]
    fn protocol(this: &WebSocketStreamOpened) -> String;

    #[wasm_bindgen(method, getter)]
    fn readable(this: &WebSocketStreamOpened) -> ReadableStream;

    #[wasm_bindgen(method, getter)]
    fn writable(this: &WebSocketStreamOpened) -> WritableStream;
}

#[wasm_bindgen]
extern "C" {
    type WebSocketStreamClosed;

    #[wasm_bindgen(method, getter)]
    fn closeCode(this: &WebSocketStreamClosed) -> u32;

    #[wasm_bindgen(method, getter)]
    fn reason(this: &WebSocketStreamClosed) -> String;
}

struct Guard {
    socket: WebSocketStream,
    closed: bool,
}

impl Guard {
    fn new(socket: WebSocketStream) -> Self {
        Self { socket, closed: false }
    }
}

impl Deref for Guard {
    type Target = WebSocketStream;
    fn deref(&self) -> &Self::Target {
        &self.socket
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if !self.closed {
            self.socket.close(&JsValue::null());
        }
    }
}

pub struct Inner {
    socket: Rc<Guard>,
    pub(crate) sender: Sender,
    pub(crate) receiver: Receiver,
}

impl Inner {
    pub async fn new(builder: WebSocketBuilder) -> io::Result<(Self, Info)> {
        let options = Object::new();

        if !builder.protocols.is_empty() {
            let arr = Array::new();
            for proto in builder.protocols {
                arr.push(&JsValue::from_str(&proto));
            }
            Reflect::set(&options, &JsValue::from_str("protocols"), &arr).unwrap();
        }

        let socket = Rc::new(Guard::new(WebSocketStream::new(&builder.url, &JsValue::from(options))));

        let opened = match socket.opened().await {
            Ok(opened) => opened,
            Err(js) => {
                return Err(Error::new(ErrorKind::ConnectionRefused, js.as_string().unwrap_or_default()));
            }
        };

        let info = Info { url: socket.url(), protocol: opened.protocol(), interface: Interface::Stream };

        let writer = opened.writable().get_writer().unwrap();
        let reader = opened.readable().get_reader().dyn_into::<ReadableStreamDefaultReader>().unwrap();

        Ok((
            Self {
                socket: socket.clone(),
                sender: Sender::new(socket.clone(), writer),
                receiver: Receiver::new(socket, reader),
            },
            info,
        ))
    }

    pub fn closed(&self) -> Closed {
        let closed = self.socket.closed();
        Closed(
            async move {
                match JsFuture::from(closed).await {
                    Ok(c) => Ok(ClosedReason {
                        code: CloseCode::from(
                            Reflect::get(&c, &JsValue::from_str("closeCode")).unwrap().as_f64().unwrap() as u16,
                        ),
                        reason: Reflect::get(&c, &JsValue::from_str("reason")).unwrap().as_string().unwrap(),
                    }),
                    Err(err) => Err(Error::new(ErrorKind::ConnectionReset, err.as_string().unwrap_or_default())),
                }
            }
            .boxed_local(),
        )
    }

    pub fn into_split(self) -> (Sender, Receiver) {
        (self.sender, self.receiver)
    }
}

pub struct Sender {
    socket: Rc<Guard>,
    writer: WritableStreamDefaultWriter,
    writing: Option<JsFuture>,
    flushing: Option<JsFuture>,
    closing: Option<JsFuture>,
}

impl Sender {
    fn new(socket: Rc<Guard>, writer: WritableStreamDefaultWriter) -> Self {
        Self { socket, writer, writing: None, flushing: None, closing: None }
    }
}

impl Sink<&JsValue> for Sender {
    type Error = io::Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        let Some(writing) = &mut self.writing else {
            return Poll::Ready(Ok(()));
        };

        let res = match ready!(writing.poll_unpin(cx)) {
            Ok(_) => Ok(()),
            Err(err) => Err(io::Error::new(ErrorKind::ConnectionReset, err.as_string().unwrap_or_default())),
        };
        self.writing = None;
        Poll::Ready(res)
    }

    fn start_send(mut self: Pin<&mut Self>, item: &JsValue) -> Result<(), Self::Error> {
        if self.writing.is_some() {
            panic!("WebSocket not ready for sending");
        }

        self.writing = Some(JsFuture::from(self.writer.write_with_chunk(item)));

        Ok(())
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        if self.flushing.is_none() {
            self.flushing = Some(JsFuture::from(self.writer.ready()));
        }

        let Some(flushing) = &mut self.flushing else { unreachable!() };
        let res = match ready!(flushing.poll_unpin(cx)) {
            Ok(_) => Ok(()),
            Err(err) => Err(io::Error::new(ErrorKind::ConnectionReset, err.as_string().unwrap_or_default())),
        };

        self.flushing = None;
        Poll::Ready(res)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        if self.closing.is_none() {
            self.closing = Some(JsFuture::from(self.writer.close()));
        }

        let Some(closing) = &mut self.closing else { unreachable!() };
        let res = match ready!(closing.poll_unpin(cx)) {
            Ok(_) => Ok(()),
            Err(err) => Err(io::Error::new(ErrorKind::ConnectionReset, err.as_string().unwrap_or_default())),
        };

        self.closing = None;
        Poll::Ready(res)
    }
}

impl Drop for Sender {
    fn drop(&mut self) {
        let writer = self.writer.clone();
        spawn_local(async move {
            let _ = JsFuture::from(writer.close());
        });
    }
}

pub struct Receiver {
    socket: Rc<Guard>,
    reader: ReadableStreamDefaultReader,
    reading: Option<JsFuture>,
}

impl Receiver {
    fn new(socket: Rc<Guard>, reader: ReadableStreamDefaultReader) -> Self {
        Self { socket, reader, reading: None }
    }
}

impl Stream for Receiver {
    type Item = io::Result<Msg>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        if self.reading.is_none() {
            self.reading = Some(JsFuture::from(self.reader.read()));
        }

        let Some(reading) = &mut self.reading else { unreachable!() };
        let res = match ready!(reading.poll_unpin(cx)) {
            Ok(data) => {
                if Reflect::get(&data, &JsValue::from_str("done")).unwrap().as_bool().unwrap() {
                    None
                } else {
                    let chunk = Reflect::get(&data, &JsValue::from_str("value")).unwrap();
                    if chunk.is_string() {
                        Some(Ok(Msg::Text(chunk.as_string().unwrap())))
                    } else {
                        let buffer = Uint8Array::new(&chunk).to_vec();
                        Some(Ok(Msg::Binary(buffer)))
                    }
                }
            }
            Err(err) => {
                Some(Err(io::Error::new(ErrorKind::ConnectionReset, err.as_string().unwrap_or_default())))
            }
        };

        self.reading = None;
        Poll::Ready(res)
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        let reader = self.reader.clone();
        spawn_local(async move {
            let _ = JsFuture::from(reader.cancel());
        });
    }
}