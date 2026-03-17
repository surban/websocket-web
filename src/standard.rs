//! Standarized WebSocket API.

use futures_core::Stream;
use futures_sink::Sink;
use futures_util::FutureExt;
use js_sys::{Array, ArrayBuffer, Promise, Uint8Array};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    future::Future,
    io,
    io::{Error, ErrorKind},
    ops::Deref,
    pin::Pin,
    rc::Rc,
    task::{ready, Context, Poll, Waker},
    time::Duration,
};
use tokio::sync::watch;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::BinaryType;

use crate::{
    closed::Closed,
    util::{js_err, sleep},
    CloseCode, ClosedReason, Info, Interface, Msg, WebSocketBuilder,
};

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

struct RecvQueue {
    msgs: RefCell<VecDeque<Msg>>,
    waker: Cell<Option<Waker>>,
    open: Cell<bool>,
    buffered: Cell<usize>,
    buffer_limit: usize,
}

impl RecvQueue {
    pub fn new(buffer_limit: usize) -> Self {
        Self {
            msgs: RefCell::new(VecDeque::new()),
            waker: Cell::new(None),
            open: Cell::new(true),
            buffered: Cell::new(0),
            buffer_limit,
        }
    }

    fn wake(&self) {
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }
    }

    pub fn close(&self) {
        self.open.set(false);
        self.wake();
    }

    pub fn enqueue(&self, msg: Msg) -> bool {
        if !self.open.get() {
            return false;
        }

        let new_buffered = self.buffered.get() + msg.len();
        if new_buffered > self.buffer_limit {
            return false;
        }

        self.buffered.set(new_buffered);
        self.msgs.borrow_mut().push_back(msg);
        self.wake();

        true
    }

    pub fn dequeue(&self) -> Option<Msg> {
        let msg = self.msgs.borrow_mut().pop_front()?;
        self.buffered.set(self.buffered.get() - msg.len());
        Some(msg)
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

        // Setup receive queue.
        let recv_queue =
            Rc::new(RecvQueue::new(builder.receive_buffer_size.unwrap_or(DEFAULT_RECEIVE_BUFFER_SIZE)));
        let (closed_tx, closed_rx) = watch::channel(None);

        // Setup close handler.
        let on_close = {
            let recv_queue = recv_queue.clone();
            let closed_tx = closed_tx.clone();
            Closure::wrap(Box::new(move |event: web_sys::CloseEvent| {
                closed_tx.send_replace(Some(ClosedReason {
                    code: event.code().into(),
                    reason: event.reason(),
                    was_clean: event.was_clean(),
                }));
                recv_queue.close();
            }) as Box<dyn Fn(_)>)
        };
        socket.set_onclose(Some(on_close.into_js_value().unchecked_ref()));

        // Setup message receive handler.
        let on_msg = {
            let recv_queue = recv_queue.clone();
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
                if !recv_queue.enqueue(msg) {
                    closed_tx.send_replace(Some(ClosedReason {
                        code: CloseCode::MessageTooBig,
                        reason: "receive buffer overflow".to_string(),
                        was_clean: false,
                    }));
                    recv_queue.close();
                }
            }) as Box<dyn Fn(_)>)
        };
        socket.set_onmessage(Some(on_msg.into_js_value().unchecked_ref()));

        // Wait for connection to be established.
        let connect = Promise::new(&mut |resolve, reject| {
            socket.set_onopen(Some(&resolve));
            socket.set_onerror(Some(&reject));
        });
        JsFuture::from(connect).await.map_err(|err| js_err(ErrorKind::ConnectionRefused, &err))?;

        Ok((
            Self {
                sender: Sender::new(socket.clone(), builder.send_buffer_size),
                receiver: Receiver::new(socket.clone(), recv_queue, closed_rx.clone()),
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

                sleep(Duration::ZERO).await;
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
        if self.writing.is_some() {
            panic!("WebSocket not ready for sending");
        }

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
        ready!(self.as_mut().poll_ready(cx))?;
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
    queue: Rc<RecvQueue>,
    closed_rx: watch::Receiver<Option<ClosedReason>>,
}

impl Receiver {
    fn new(socket: Rc<Guard>, queue: Rc<RecvQueue>, closed_rx: watch::Receiver<Option<ClosedReason>>) -> Self {
        Self { _socket: socket, queue, closed_rx }
    }
}

impl Stream for Receiver {
    type Item = io::Result<Msg>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        if let Some(msg) = self.queue.dequeue() {
            return Poll::Ready(Some(Ok(msg)));
        }

        if !self.queue.open.get() {
            return match &*self.closed_rx.borrow() {
                Some(reason) if reason.was_clean => Poll::Ready(None),
                Some(reason) => {
                    Poll::Ready(Some(Err(Error::new(ErrorKind::ConnectionReset, reason.reason.clone()))))
                }
                None => Poll::Ready(Some(Err(Error::new(ErrorKind::ConnectionReset, "WebSocket closed")))),
            };
        }

        self.queue.waker.set(Some(cx.waker().clone()));
        Poll::Pending
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        // empty
    }
}
