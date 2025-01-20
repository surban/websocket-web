//! WebSocket streams API.

use futures_core::Stream;
use futures_sink::Sink;
use futures_util::FutureExt;
use js_sys::{Array, Object, Promise, Reflect, Uint8Array};
use std::{
    cell::Cell,
    io,
    io::ErrorKind,
    ops::Deref,
    pin::Pin,
    rc::Rc,
    task::{ready, Context, Poll},
};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{ReadableStream, ReadableStreamDefaultReader, WritableStream, WritableStreamDefaultWriter};

use crate::{
    closed::{CloseCode, Closed, ClosedReason},
    util::{js_err, js_err_msg},
    Info, Interface, Msg, WebSocketBuilder,
};

#[wasm_bindgen]
extern "C" {
    type WebSocketStream;

    #[wasm_bindgen(constructor)]
    fn new(url: &str, options: &JsValue) -> WebSocketStream;

    #[wasm_bindgen(method, getter, catch)]
    async fn opened(this: &WebSocketStream) -> Result<WebSocketStreamOpened, JsValue>;

    #[wasm_bindgen(method, getter)]
    fn closed(this: &WebSocketStream) -> Promise;

    #[wasm_bindgen(method, catch)]
    fn close(this: &WebSocketStream, options: &JsValue) -> Result<(), JsValue>;

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
    closed: Cell<bool>,
}

impl Guard {
    fn new(socket: WebSocketStream) -> Self {
        Self { socket, closed: Cell::new(false) }
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
        if !self.closed.get() {
            self.socket.close(&JsValue::null()).unwrap();
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
        // Create WebSocketStream.
        let options = Object::new();
        if !builder.protocols.is_empty() {
            let arr = Array::new();
            for proto in builder.protocols {
                arr.push(&JsValue::from_str(&proto));
            }
            Reflect::set(&options, &JsValue::from_str("protocols"), &arr).unwrap();
        }
        let socket = Rc::new(Guard::new(WebSocketStream::new(&builder.url, &JsValue::from(options))));

        // Open WebSocket connection.
        let opened = match socket.opened().await {
            Ok(opened) => opened,
            Err(js) => return Err(js_err(ErrorKind::ConnectionRefused, &js)),
        };

        // Obtain reader and writer.
        let writer = opened.writable().get_writer().unwrap();
        let reader = opened.readable().get_reader().dyn_into::<ReadableStreamDefaultReader>().unwrap();

        Ok((
            Self {
                socket: socket.clone(),
                sender: Sender::new(socket.clone(), writer),
                receiver: Receiver::new(socket.clone(), reader),
            },
            Info { url: socket.url(), protocol: opened.protocol(), interface: Interface::Stream },
        ))
    }

    pub fn closed(&self) -> Closed {
        let closed = self.socket.closed();
        Closed(
            async move {
                match JsFuture::from(closed).await {
                    Ok(c) => ClosedReason {
                        code: CloseCode::from(
                            Reflect::get(&c, &JsValue::from_str("closeCode")).unwrap().as_f64().unwrap() as u16,
                        ),
                        reason: Reflect::get(&c, &JsValue::from_str("reason")).unwrap().as_string().unwrap(),
                        was_clean: true,
                    },
                    Err(err) => ClosedReason {
                        code: CloseCode::AbnormalClosure,
                        reason: js_err_msg(&err).unwrap_or_default(),
                        was_clean: false,
                    },
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

    #[track_caller]
    pub fn close(self, code: u16, reason: &str) {
        let options = Object::new();
        Reflect::set(&options, &JsValue::from("closeCode"), &JsValue::from(code)).unwrap();
        Reflect::set(&options, &JsValue::from("reason"), &JsValue::from_str(reason)).unwrap();
        self.socket.close(&options).unwrap();
        self.socket.closed.set(true);
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
            Err(err) => Err(js_err(ErrorKind::ConnectionReset, &err)),
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
            Err(err) => Err(js_err(ErrorKind::ConnectionReset, &err)),
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
            Err(err) => Err(js_err(ErrorKind::ConnectionReset, &err)),
        };

        self.closing = None;
        Poll::Ready(res)
    }
}

impl Drop for Sender {
    fn drop(&mut self) {
        // empty
    }
}

pub struct Receiver {
    _socket: Rc<Guard>,
    reader: ReadableStreamDefaultReader,
    reading: Option<JsFuture>,
}

impl Receiver {
    fn new(socket: Rc<Guard>, reader: ReadableStreamDefaultReader) -> Self {
        Self { _socket: socket, reader, reading: None }
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
            Err(err) => Some(Err(js_err(ErrorKind::ConnectionReset, &err))),
        };

        self.reading = None;
        Poll::Ready(res)
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        // empty
    }
}
