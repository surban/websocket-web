//! # WebSockets on the web
//!
//! This crate provides WebSocket support in a JavaScript runtime environment, usually a web browser.
//!
//! If available it uses the experimental [WebSocketStream API](https://developer.mozilla.org/en-US/docs/Web/API/WebSocketStream),
//! otherwise the standardized [WebSocket API](https://developer.mozilla.org/en-US/docs/Web/API/WebSockets_API)
//! is used.
//!
//! The WebSocketStream API provides backpressure in both transmit and receive directions.
//! The standardized WebSocket API provides backpressure only in the transmit direction;
//! if the receive buffer overflows, the WebSocket is closed and an error is returned.
//!
//! ## Sending WebSocket messages
//!
//! WebSocket is a message oriented protocol, i.e. it deals with sending and receiving discrete
//! messages rather than streams of data. A message can be either text (UTF-8 string) or binary.
//!
//! [WebSocket] and [WebSocketSender] implement the [Sink] trait for sending messages.
//! A Rust [String] or [`&str`](str) is sent as a text message and
//! a [`Vec<u8>`] or `&[u8]` is transmitted as a binary message.
//!
//! Additionally, both types implement [AsyncWrite]. When using this trait, each write
//! is sent as a binary message containg the whole buffer.
//!
//! ## Receiving WebSocket messages
//!
//! [WebSocket] and [WebSocketReceiver] implement the [Stream] trait for receiving messages.
//! The received data type is [`Msg`], which can either be [text](Msg::Text) or [binary](Msg::Binary).
//!
//! Additionally, both types implement [AsyncRead]. When using this trait, each received
//! message is converted to binary format and buffered to support partial reads, i.e.
//! a read using a buffer with a size smaller than the received message.
//!
//! ## Example
//!
//! The following example establishes a WebSocket connection to `localhost` on port `8765`.
//! It then sends the text message `Test123` and then receiving one incoming message.
//! Finally, it explicitly closes the WebSocket with the reason `Goodbye!`.
//!
//! ```
//! use websocket_web::{WebSocket, CloseCode};
//! use futures_util::{SinkExt, StreamExt};
//!
//! // Connect to WebSocket echo server running on localhost.
//! let mut socket = WebSocket::connect("ws://127.0.0.1:8765").await.unwrap();
//!
//! // Send WebSocket text message.
//! socket.send("Test123").await.unwrap();
//!
//! // Receive WebSocket message.
//! let msg = socket.next().await.unwrap().unwrap();
//! assert_eq!(msg.to_string(), "Test123");
//!
//! // Explicitly close WebSocket with close code and reason (optional).
//! socket.close_with_reason(CloseCode::NormalClosure, "Goodbye!");
//! ```

#![warn(missing_docs)]
#[cfg(not(target_family = "wasm"))]
compile_error!("websocket-web requires a WebAssembly target");

mod closed;
mod standard;
mod stream;
mod util;

use futures_core::Stream;
use futures_sink::Sink;
use futures_util::{SinkExt, StreamExt};
use js_sys::{Reflect, Uint8Array};
use std::{
    fmt, io,
    io::ErrorKind,
    mem,
    pin::Pin,
    rc::Rc,
    task::{ready, Context, Poll},
};
use tokio::io::{AsyncRead, AsyncWrite};
use wasm_bindgen::prelude::*;

pub use closed::{CloseCode, Closed, ClosedReason};

/// The WebSocket API used to interact with the JavaScript runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interface {
    /// Experimental [WebSocketStream](https://developer.mozilla.org/en-US/docs/Web/API/WebSocketStream) interface.
    ///
    /// This provides backpressure in both directions and is recommend if supported by the browser.
    Stream,
    /// Standarized [WebSocket](https://developer.mozilla.org/en-US/docs/Web/API/WebSocket) interface.
    ///
    /// This provides backpressure only in the transmit direction and should only be used as a fallback.
    Standard,
}

impl Interface {
    /// Whether the interface is supported by the current runtime.
    pub fn is_supported(&self) -> bool {
        let global = js_sys::global();
        match self {
            Self::Stream => Reflect::has(&global, &JsValue::from_str("WebSocketStream")).unwrap_or_default(),
            Self::Standard => Reflect::has(&global, &JsValue::from_str("WebSocket")).unwrap_or_default(),
        }
    }
}

/// A WebSocket message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Msg {
    /// Text message.
    Text(String),
    /// Binary message.
    Binary(Vec<u8>),
}

impl Msg {
    /// Whether this is a text message.
    pub const fn is_text(&self) -> bool {
        matches!(self, Self::Text(_))
    }

    /// Whether this is a binary message.
    pub const fn is_binary(&self) -> bool {
        matches!(self, Self::Binary(_))
    }

    /// Convert to binary message.
    pub fn to_vec(self) -> Vec<u8> {
        match self {
            Self::Text(text) => text.as_bytes().to_vec(),
            Self::Binary(vec) => vec,
        }
    }

    /// Length of message in bytes.
    pub fn len(&self) -> usize {
        match self {
            Self::Text(text) => text.len(),
            Self::Binary(vec) => vec.len(),
        }
    }

    /// Whether the length of this message is zero.
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Text(text) => text.is_empty(),
            Self::Binary(vec) => vec.is_empty(),
        }
    }
}

impl fmt::Display for Msg {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Text(text) => write!(f, "{text}"),
            Self::Binary(binary) => write!(f, "{}", String::from_utf8_lossy(binary)),
        }
    }
}

impl From<Msg> for Vec<u8> {
    fn from(msg: Msg) -> Self {
        msg.to_vec()
    }
}

impl AsRef<[u8]> for Msg {
    fn as_ref(&self) -> &[u8] {
        match self {
            Self::Text(text) => text.as_bytes(),
            Self::Binary(vec) => vec,
        }
    }
}

/// Builder for connecting a WebSocket.
#[derive(Debug, Clone)]
pub struct WebSocketBuilder {
    url: String,
    protocols: Vec<String>,
    interface: Option<Interface>,
    send_buffer_size: Option<usize>,
    receive_buffer_size: Option<usize>,
}

impl WebSocketBuilder {
    /// Creates a new WebSocket builder that will connect to the specified URL.
    pub fn new(url: impl AsRef<str>) -> Self {
        Self {
            url: url.as_ref().to_string(),
            protocols: Vec::new(),
            interface: None,
            send_buffer_size: None,
            receive_buffer_size: None,
        }
    }

    /// Sets the WebSocket browser interface to use.
    ///
    /// If unset, the stream-based interface is preferred when available.
    pub fn set_interface(&mut self, interface: Interface) {
        self.interface = Some(interface);
    }

    /// Sets the sub-protocol(s) that the client would like to use.
    ///
    /// Subprotocols may be selected from the [IANA WebSocket Subprotocol Name Registry]
    /// or may be custom names jointly understood by the client and the server.
    /// A single server can implement multiple WebSocket sub-protocols, and
    /// handle different types of interactions depending on the specified value.
    ///
    /// If protocols is included, the connection will only be established if the server
    /// reports that it has selected one of these sub-protocols.
    ///
    /// [IANA WebSocket Subprotocol Name Registry]: https://www.iana.org/assignments/websocket/websocket.xml#subprotocol-name
    pub fn set_protocols<P>(&mut self, protocols: impl IntoIterator<Item = P>)
    where
        P: AsRef<str>,
    {
        self.protocols = protocols.into_iter().map(|s| s.as_ref().to_string()).collect();
    }

    /// Sets the maximum send buffer size in bytes.
    ///
    /// This only affects the [standard WebSocket interface](Interface::Standard).
    ///
    /// If the maximum send buffer size is reached, all sending function stop
    /// accepting data until the send buffer size falls below the specified size.
    pub fn set_send_buffer_size(&mut self, send_buffer_size: usize) {
        self.send_buffer_size = Some(send_buffer_size);
    }

    /// Sets the maximum receive buffer size in bytes.
    ///
    /// This only affects the [standard WebSocket interface](Interface::Standard).
    ///
    /// If the maximum receive buffer size is reached, the WebSocket is closed and an
    /// error is returned when trying to read from it.
    pub fn set_receive_buffer_size(&mut self, receive_buffer_size: usize) {
        self.receive_buffer_size = Some(receive_buffer_size);
    }

    /// Establishes the WebSocket connection.
    pub async fn connect(self) -> io::Result<WebSocket> {
        let interface = match self.interface {
            Some(interface) => interface,
            None if Interface::Stream.is_supported() => Interface::Stream,
            None => Interface::Standard,
        };

        if !interface.is_supported() {
            match interface {
                Interface::Stream => {
                    return Err(io::Error::new(ErrorKind::Unsupported, "WebSocketStream not supported"))
                }
                Interface::Standard => {
                    return Err(io::Error::new(ErrorKind::Unsupported, "WebSocket not supported"))
                }
            }
        }

        match interface {
            Interface::Stream => {
                let (stream, info) = stream::Inner::new(self).await?;
                Ok(WebSocket { inner: Inner::Stream(stream), info: Rc::new(info), read_buf: Vec::new() })
            }
            Interface::Standard => {
                let (standard, info) = standard::Inner::new(self).await?;
                Ok(WebSocket { inner: Inner::Standard(standard), info: Rc::new(info), read_buf: Vec::new() })
            }
        }
    }
}

struct Info {
    url: String,
    protocol: String,
    interface: Interface,
}

/// A WebSocket provided by the JavaScript runtime (usually the web browser).
///
/// The WebSocket is closed when dropped.
pub struct WebSocket {
    inner: Inner,
    info: Rc<Info>,
    read_buf: Vec<u8>,
}

enum Inner {
    Stream(stream::Inner),
    Standard(standard::Inner),
}

impl fmt::Debug for WebSocket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebSocket")
            .field("url", &self.info.url)
            .field("protocol", &self.protocol())
            .field("interface", &self.interface())
            .finish()
    }
}

impl WebSocket {
    /// Connect to the specified WebSocket URL using default options.
    pub async fn connect(url: impl AsRef<str>) -> io::Result<Self> {
        WebSocketBuilder::new(url).connect().await
    }

    /// The URL of the WebSocket server.
    pub fn url(&self) -> &str {
        &self.info.url
    }

    /// A string representing the sub-protocol used to open the current WebSocket connection
    /// (chosen from the options specified in the [WebSocketBuilder]).
    ///
    /// Returns an empty string if no sub-protocol has been used to open the connection
    /// (i.e. no sub-protocol options were specified in the [WebSocketBuilder]).
    pub fn protocol(&self) -> &str {
        &self.info.protocol
    }

    /// The used WebSocket browser interface.
    pub fn interface(&self) -> Interface {
        self.info.interface
    }

    /// Splits this WebSocket into a sender and receiver.
    pub fn into_split(self) -> (WebSocketSender, WebSocketReceiver) {
        let Self { inner, info, read_buf } = self;
        match inner {
            Inner::Stream(inner) => {
                let (sender, receiver) = inner.into_split();
                let sender = WebSocketSender { inner: SenderInner::Stream(sender), info: info.clone() };
                let receiver = WebSocketReceiver { inner: ReceiverInner::Stream(receiver), info, read_buf };
                (sender, receiver)
            }
            Inner::Standard(inner) => {
                let (sender, receiver) = inner.into_split();
                let sender = WebSocketSender { inner: SenderInner::Standard(sender), info: info.clone() };
                let receiver =
                    WebSocketReceiver { inner: ReceiverInner::Standard(receiver), info, read_buf: Vec::new() };
                (sender, receiver)
            }
        }
    }

    /// Closes the WebSocket.
    pub fn close(self) {
        self.into_split().0.close();
    }

    /// Closes the WebSocket with the specified close code and reason.
    ///
    /// ## Panics
    /// Panics if the close code is neither [CloseCode::NormalClosure] nor
    /// [CloseCode::Other] with a value between 3000 and 4999.
    #[track_caller]
    pub fn close_with_reason(self, code: CloseCode, reason: &str) {
        self.into_split().0.close_with_reason(code, reason);
    }

    /// Returns a future that resolves when the WebSocket is closed remotely.
    pub fn closed(&self) -> Closed {
        match &self.inner {
            Inner::Stream(inner) => inner.closed(),
            Inner::Standard(inner) => inner.closed(),
        }
    }

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), io::Error>> {
        match &mut self.inner {
            Inner::Stream(inner) => inner.sender.poll_ready_unpin(cx),
            Inner::Standard(inner) => inner.sender.poll_ready_unpin(cx),
        }
    }

    fn start_send(mut self: Pin<&mut Self>, item: &JsValue) -> Result<(), io::Error> {
        match &mut self.inner {
            Inner::Stream(inner) => inner.sender.start_send_unpin(item),
            Inner::Standard(inner) => inner.sender.start_send_unpin(item),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), io::Error>> {
        match &mut self.inner {
            Inner::Stream(inner) => inner.sender.poll_flush_unpin(cx),
            Inner::Standard(inner) => inner.sender.poll_flush_unpin(cx),
        }
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), io::Error>> {
        match &mut self.inner {
            Inner::Stream(inner) => inner.sender.poll_close_unpin(cx),
            Inner::Standard(inner) => inner.sender.poll_close_unpin(cx),
        }
    }
}

impl Sink<&str> for WebSocket {
    type Error = io::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        self.poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: &str) -> Result<(), Self::Error> {
        self.start_send(&JsValue::from_str(item))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        self.poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        self.poll_close(cx)
    }
}

impl Sink<String> for WebSocket {
    type Error = io::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        self.poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: String) -> Result<(), Self::Error> {
        self.start_send(&JsValue::from_str(&item))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        self.poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        self.poll_close(cx)
    }
}

impl Sink<&[u8]> for WebSocket {
    type Error = io::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        self.poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: &[u8]) -> Result<(), Self::Error> {
        self.start_send(&Uint8Array::from(item))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        self.poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        self.poll_close(cx)
    }
}

impl Sink<Vec<u8>> for WebSocket {
    type Error = io::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        self.poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: Vec<u8>) -> Result<(), Self::Error> {
        self.start_send(&Uint8Array::from(&item[..]))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        self.poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        self.poll_close(cx)
    }
}

impl Sink<Msg> for WebSocket {
    type Error = io::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        self.poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: Msg) -> Result<(), Self::Error> {
        match item {
            Msg::Text(text) => self.start_send(&JsValue::from_str(&text)),
            Msg::Binary(vec) => self.start_send(&Uint8Array::from(&vec[..])),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        self.poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        self.poll_close(cx)
    }
}

impl AsyncWrite for WebSocket {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context, buf: &[u8]) -> Poll<Result<usize, io::Error>> {
        ready!(self.as_mut().poll_ready(cx))?;
        self.start_send(&Uint8Array::from(buf))?;
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), io::Error>> {
        self.poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), io::Error>> {
        self.poll_close(cx)
    }
}

impl Stream for WebSocket {
    type Item = io::Result<Msg>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        match &mut self.inner {
            Inner::Stream(inner) => inner.receiver.poll_next_unpin(cx),
            Inner::Standard(inner) => inner.receiver.poll_next_unpin(cx),
        }
    }
}

impl AsyncRead for WebSocket {
    fn poll_read(
        mut self: Pin<&mut Self>, cx: &mut Context, buf: &mut tokio::io::ReadBuf,
    ) -> Poll<io::Result<()>> {
        while self.read_buf.is_empty() {
            let Some(msg) = ready!(self.as_mut().poll_next(cx)?) else { return Poll::Ready(Ok(())) };
            self.read_buf = msg.to_vec();
        }

        let part = if buf.remaining() < self.read_buf.len() {
            let rem = self.read_buf.split_off(buf.remaining());
            mem::replace(&mut self.read_buf, rem)
        } else {
            mem::take(&mut self.read_buf)
        };

        buf.put_slice(&part);
        Poll::Ready(Ok(()))
    }
}

/// Sending part of a [WebSocket].
///
/// The WebSocket is closed when both the [WebSocketSender] and [WebSocketReceiver]
/// are dropped.
pub struct WebSocketSender {
    inner: SenderInner,
    info: Rc<Info>,
}

enum SenderInner {
    Stream(stream::Sender),
    Standard(standard::Sender),
}

impl fmt::Debug for WebSocketSender {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebSocketSender")
            .field("url", &self.info.url)
            .field("protocol", &self.protocol())
            .field("interface", &self.interface())
            .finish()
    }
}

impl WebSocketSender {
    /// The URL of the WebSocket server.
    pub fn url(&self) -> &str {
        &self.info.url
    }

    /// A string representing the sub-protocol used to open the current WebSocket connection.
    pub fn protocol(&self) -> &str {
        &self.info.protocol
    }

    /// The used WebSocket browser interface.
    pub fn interface(&self) -> Interface {
        self.info.interface
    }

    /// Closes the WebSocket.
    ///
    /// This also closes the corresponding [WebSocketReceiver].
    pub fn close(self) {
        self.close_with_reason(CloseCode::NormalClosure, "");
    }

    /// Closes the WebSocket with the specified close code and reason.
    ///
    /// This also closes the corresponding [WebSocketReceiver].
    ///
    /// ## Panics
    /// Panics if the close code is neither [CloseCode::NormalClosure] nor
    /// [CloseCode::Other] with a value between 3000 and 4999.
    #[track_caller]
    pub fn close_with_reason(self, code: CloseCode, reason: &str) {
        if !code.is_valid() {
            panic!("WebSocket close code {code} is invalid");
        }

        match self.inner {
            SenderInner::Stream(sender) => sender.close(code.into(), reason),
            SenderInner::Standard(sender) => sender.close(code.into(), reason),
        }
    }

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), io::Error>> {
        match &mut self.inner {
            SenderInner::Stream(inner) => inner.poll_ready_unpin(cx),
            SenderInner::Standard(inner) => inner.poll_ready_unpin(cx),
        }
    }

    fn start_send(mut self: Pin<&mut Self>, item: &JsValue) -> Result<(), io::Error> {
        match &mut self.inner {
            SenderInner::Stream(inner) => inner.start_send_unpin(item),
            SenderInner::Standard(inner) => inner.start_send_unpin(item),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), io::Error>> {
        match &mut self.inner {
            SenderInner::Stream(inner) => inner.poll_flush_unpin(cx),
            SenderInner::Standard(inner) => inner.poll_flush_unpin(cx),
        }
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), io::Error>> {
        match &mut self.inner {
            SenderInner::Stream(inner) => inner.poll_close_unpin(cx),
            SenderInner::Standard(inner) => inner.poll_close_unpin(cx),
        }
    }
}

impl Sink<&str> for WebSocketSender {
    type Error = io::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        self.poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: &str) -> Result<(), Self::Error> {
        self.start_send(&JsValue::from_str(item))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        self.poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        self.poll_close(cx)
    }
}

impl Sink<String> for WebSocketSender {
    type Error = io::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        self.poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: String) -> Result<(), Self::Error> {
        self.start_send(&JsValue::from_str(&item))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        self.poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        self.poll_close(cx)
    }
}

impl Sink<&[u8]> for WebSocketSender {
    type Error = io::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        self.poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: &[u8]) -> Result<(), Self::Error> {
        self.start_send(&Uint8Array::from(item))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        self.poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        self.poll_close(cx)
    }
}

impl Sink<Vec<u8>> for WebSocketSender {
    type Error = io::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        self.poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: Vec<u8>) -> Result<(), Self::Error> {
        self.start_send(&Uint8Array::from(&item[..]))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        self.poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        self.poll_close(cx)
    }
}

impl Sink<Msg> for WebSocketSender {
    type Error = io::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        self.poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: Msg) -> Result<(), Self::Error> {
        match item {
            Msg::Text(text) => self.start_send(&JsValue::from_str(&text)),
            Msg::Binary(vec) => self.start_send(&Uint8Array::from(&vec[..])),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        self.poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        self.poll_close(cx)
    }
}

impl AsyncWrite for WebSocketSender {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context, buf: &[u8]) -> Poll<Result<usize, io::Error>> {
        ready!(self.as_mut().poll_ready(cx))?;
        self.start_send(&Uint8Array::from(buf))?;
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), io::Error>> {
        self.poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), io::Error>> {
        self.poll_close(cx)
    }
}

/// Receiving part of a [WebSocket].
///
/// The WebSocket is closed when both the [WebSocketSender] and [WebSocketReceiver]
/// are dropped.
pub struct WebSocketReceiver {
    inner: ReceiverInner,
    info: Rc<Info>,
    read_buf: Vec<u8>,
}

enum ReceiverInner {
    Stream(stream::Receiver),
    Standard(standard::Receiver),
}

impl fmt::Debug for WebSocketReceiver {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("WebSocketReceiver")
            .field("url", &self.info.url)
            .field("protocol", &self.protocol())
            .field("interface", &self.interface())
            .finish()
    }
}

impl WebSocketReceiver {
    /// The URL of the WebSocket server.
    pub fn url(&self) -> &str {
        &self.info.url
    }

    /// A string representing the sub-protocol used to open the current WebSocket connection.
    pub fn protocol(&self) -> &str {
        &self.info.protocol
    }

    /// The used WebSocket browser interface.
    pub fn interface(&self) -> Interface {
        self.info.interface
    }
}

impl Stream for WebSocketReceiver {
    type Item = io::Result<Msg>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        match &mut self.inner {
            ReceiverInner::Stream(inner) => inner.poll_next_unpin(cx),
            ReceiverInner::Standard(inner) => inner.poll_next_unpin(cx),
        }
    }
}

impl AsyncRead for WebSocketReceiver {
    fn poll_read(
        mut self: Pin<&mut Self>, cx: &mut Context, buf: &mut tokio::io::ReadBuf,
    ) -> Poll<io::Result<()>> {
        while self.read_buf.is_empty() {
            let Some(msg) = ready!(self.as_mut().poll_next(cx)?) else { return Poll::Ready(Ok(())) };
            self.read_buf = msg.to_vec();
        }

        let part = if buf.remaining() < self.read_buf.len() {
            let rem = self.read_buf.split_off(buf.remaining());
            mem::replace(&mut self.read_buf, rem)
        } else {
            mem::take(&mut self.read_buf)
        };

        buf.put_slice(&part);
        Poll::Ready(Ok(()))
    }
}
