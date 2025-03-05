//! Standarized WebSocket API.

use futures_channel::mpsc;
use futures_core::Stream;
use futures_sink::Sink;
use futures_util::{FutureExt, StreamExt};
use js_sys::{Array, ArrayBuffer, Promise, Uint8Array};
use std::{
    cell::{Cell, RefCell},
    future::Future,
    io,
    io::{Error, ErrorKind},
    ops::Deref,
    pin::Pin,
    rc::Rc,
    task::{ready, Context, Poll},
    time::Duration,
};
use tokio::sync::{watch, Semaphore};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::BinaryType;

use crate::{
    closed::Closed,
    util::{js_err, sleep},
    CloseCode, ClosedReason, Info, Interface, Msg, WebSocketBuilder,
};

const SEND_BUFFER_CHECK_INTERVAL: Duration = Duration::from_millis(1);
const DEFAULT_SEND_BUFFER_SIZE: usize = 4_194_304;
const DEFAULT_RECEIVE_BUFFER_SIZE: usize = 67_108_864;

struct Guard {
    socket: web_sys::WebSocket,
    closed: Cell<bool>,
}

impl Guard {
    fn new(socket: web_sys::WebSocket) -> Self {
        Self { socket, closed: Cell::new(false) }
    }
}

impl Deref for Guard {
    type Target = web_sys::WebSocket;
    fn deref(&self) -> &Self::Target {
        &self.socket
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if !self.closed.get() {
            let _ = self.socket.close();
        }
    }
}

pub struct Inner {
    pub(crate) sender: Sender,
    pub(crate) receiver: Receiver,
    closed_rx: watch::Receiver<Option<ClosedReason>>,
}

impl Inner {
    pub async fn new(builder: WebSocketBuilder) -> io::Result<(Self, Info)> {
        // Create WebSocket.
        let protocols = Array::new();
        for proto in builder.protocols {
            protocols.push(&JsValue::from_str(&proto));
        }
        let socket = Rc::new(Guard::new(
            web_sys::WebSocket::new_with_str_sequence(&builder.url, &protocols)
                .map_err(|err| js_err(ErrorKind::InvalidInput, &err))?,
        ));
        socket.set_binary_type(BinaryType::Arraybuffer);

        // Wait for connection to be established.
        let connect = Promise::new(&mut |resolve, reject| {
            socket.set_onopen(Some(&resolve));
            socket.set_onerror(Some(&reject));
        });
        JsFuture::from(connect).await.map_err(|err| js_err(ErrorKind::ConnectionRefused, &err))?;

        // Setup channel.
        let (tx, rx) = mpsc::unbounded();
        let tx = Rc::new(RefCell::new(Some(tx)));
        let (closed_tx, closed_rx) = watch::channel(None);
        let buffered =
            Rc::new(Semaphore::new(builder.receive_buffer_size.unwrap_or(DEFAULT_RECEIVE_BUFFER_SIZE)));

        // Setup close handler.
        let on_close = {
            let tx = tx.clone();
            let closed_tx = closed_tx.clone();
            Closure::wrap(Box::new(move |event: web_sys::CloseEvent| {
                closed_tx.send_replace(Some(ClosedReason {
                    code: event.code().into(),
                    reason: event.reason(),
                    was_clean: event.was_clean(),
                }));
                tx.replace(None);
            }) as Box<dyn Fn(_)>)
        };
        socket.set_onclose(Some(on_close.into_js_value().unchecked_ref()));

        // Setup message receive handler.
        let on_msg = {
            let buffered = buffered.clone();
            Closure::wrap(Box::new(move |event: web_sys::MessageEvent| {
                let msg = {
                    let data = event.data();
                    if let Some(buf) = data.dyn_ref::<ArrayBuffer>() {
                        Msg::Binary(js_sys::Uint8Array::new(buf).to_vec())
                    } else if let Some(text) = data.as_string() {
                        Msg::Text(text)
                    } else {
                        unreachable!("received event with unknown data type");
                    }
                };
                match u32::try_from(msg.len()).ok().and_then(|len| buffered.try_acquire_many(len).ok()) {
                    Some(permit) => {
                        // Permits will be added back when message is dequeued by receiver.
                        let tx = tx.borrow();
                        let Some(tx) = &*tx else { return };
                        let _ = tx.unbounded_send(msg);
                        permit.forget();
                    }
                    None => {
                        closed_tx.send_replace(Some(ClosedReason {
                            code: CloseCode::MessageTooBig,
                            reason: "receive buffer overflow".to_string(),
                            was_clean: false,
                        }));
                        tx.replace(None);
                    }
                }
            }) as Box<dyn Fn(_)>)
        };
        socket.set_onmessage(Some(on_msg.into_js_value().unchecked_ref()));

        Ok((
            Self {
                sender: Sender::new(socket.clone(), builder.send_buffer_size),
                receiver: Receiver::new(socket.clone(), rx, closed_rx.clone(), buffered),
                closed_rx,
            },
            Info { url: builder.url, protocol: socket.protocol(), interface: Interface::Standard },
        ))
    }

    pub fn closed(&self) -> Closed {
        let mut closed_rx = self.closed_rx.clone();
        Closed(
            async move {
                match closed_rx.wait_for(|c| c.is_some()).await {
                    Ok(reason) => reason.clone().unwrap(),
                    Err(_) => {
                        ClosedReason { code: CloseCode::AbnormalClosure, reason: String::new(), was_clean: false }
                    }
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
    send_buffer_size: usize,
    writing: Option<Pin<Box<dyn Future<Output = io::Result<()>>>>>,
}

impl Sender {
    fn new(socket: Rc<Guard>, send_buffer_size: Option<usize>) -> Self {
        Self { socket, send_buffer_size: send_buffer_size.unwrap_or(DEFAULT_SEND_BUFFER_SIZE), writing: None }
    }

    #[track_caller]
    pub fn close(self, code: u16, reason: &str) {
        self.socket.close_with_code_and_reason(code, reason).unwrap();
        self.socket.closed.set(true);
    }

    fn wait_for_buffered_amount(&self, max_amount: usize) -> impl Future<Output = io::Result<()>> {
        let socket = self.socket.clone();
        async move {
            loop {
                if socket.ready_state() != web_sys::WebSocket::OPEN {
                    return Err(Error::new(ErrorKind::ConnectionReset, "WebSocket not open"));
                }

                if usize::try_from(socket.buffered_amount()).unwrap() <= max_amount {
                    return Ok(());
                }

                sleep(SEND_BUFFER_CHECK_INTERVAL).await;
            }
        }
    }
}

impl Sink<&JsValue> for Sender {
    type Error = io::Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        if self.writing.is_none() {
            self.writing = Some(Box::pin(self.wait_for_buffered_amount(self.send_buffer_size)));
        }

        let Some(writing) = &mut self.writing else { unreachable!() };

        let res = ready!(writing.poll_unpin(cx));
        self.writing = None;
        Poll::Ready(res)
    }

    fn start_send(self: Pin<&mut Self>, item: &JsValue) -> Result<(), Self::Error> {
        if let Some(array) = item.dyn_ref::<Uint8Array>() {
            self.socket.send_with_js_u8_array(array)
        } else if let Some(str) = item.as_string() {
            self.socket.send_with_str(&str)
        } else {
            unreachable!()
        }
        .map_err(|err| js_err(ErrorKind::ConnectionReset, &err))?;

        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        ready!(self.as_mut().poll_flush(cx))?;
        let res = self.socket.close().map_err(|err| js_err(ErrorKind::ConnectionReset, &err));
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
    rx: mpsc::UnboundedReceiver<Msg>,
    closed_rx: watch::Receiver<Option<ClosedReason>>,
    buffered: Rc<Semaphore>,
}

impl Receiver {
    fn new(
        socket: Rc<Guard>, rx: mpsc::UnboundedReceiver<Msg>, closed_rx: watch::Receiver<Option<ClosedReason>>,
        buffered: Rc<Semaphore>,
    ) -> Self {
        Self { _socket: socket, rx, closed_rx, buffered }
    }
}

impl Stream for Receiver {
    type Item = io::Result<Msg>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        match ready!(self.rx.poll_next_unpin(cx)) {
            Some(msg) => {
                self.buffered.add_permits(msg.len());
                Poll::Ready(Some(Ok(msg)))
            }
            None => match &*self.closed_rx.borrow() {
                Some(reason) if reason.was_clean => Poll::Ready(None),
                Some(reason) => {
                    Poll::Ready(Some(Err(Error::new(ErrorKind::ConnectionReset, reason.reason.clone()))))
                }
                None => Poll::Ready(Some(Err(Error::new(ErrorKind::ConnectionReset, "WebSocket closed")))),
            },
        }
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        // empty
    }
}
