//! # WebSockets on the web
//!
//! This provides WebSocket support in JavaScript runtime environment (usually web browsers).
//!
//! If available it uses the experimental [`WebSocketStream`](https://developer.mozilla.org/en-US/docs/Web/API/WebSocketStream),
//! otherwise the standardized [WebSocket API](https://developer.mozilla.org/en-US/docs/Web/API/WebSockets_API)
//! is used.
//!

mod closed;
mod standard;
mod stream;

#[cfg(test)]
mod tests;

use futures::prelude::*;
use js_sys::{Array, Object, Promise, Reflect, Uint8Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{ReadableStream, ReadableStreamDefaultReader, WritableStream};

use std::io::Error;
use std::io::ErrorKind;
use std::ops::Deref;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::{fmt, io};

pub use closed::{CloseCode, ClosedReason};

/// The WebSocket interface used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interface {
    /// Experimental [WebSocketStream](https://developer.mozilla.org/en-US/docs/Web/API/WebSocketStream) interface.
    ///
    /// This provides backpressure and is recommend if supported by the browser.
    Stream,
    /// Standarized [WebSocket](https://developer.mozilla.org/en-US/docs/Web/API/WebSocket) interface.
    ///
    /// This provides no backpressure and should only be used as a fallback.
    Standard,
}

/// A message received over a WebSocket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Msg {
    /// Text message.
    Text(String),
    /// Binary message.
    Binary(Vec<u8>),
}

/// Builder for connecting a WebSocket.
#[derive(Debug, Clone)]
pub struct WebSocketBuilder {
    url: String,
    protocols: Vec<String>,
    interface: Option<Interface>,
}

impl WebSocketBuilder {
    /// Creates a new WebSocket builder that will connect to the specified URL.
    pub fn new(url: impl AsRef<str>) -> Self {
        Self { url: url.as_ref().to_string(), protocols: Vec::new(), interface: None }
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

    /// Establishes the WebSocket connection.
    pub async fn connect(self) -> io::Result<WebSocket> {
        let global = js_sys::global();
        let has_web_socket_stream =
            Reflect::has(&global, &JsValue::from_str("WebSocketStream")).unwrap_or_default();
        let has_web_socket = Reflect::has(&global, &JsValue::from_str("WebSocket")).unwrap_or_default();

        let interface = match self.interface {
            Some(interface) => interface,
            None if has_web_socket_stream => Interface::Stream,
            None => Interface::Standard,
        };

        match interface {
            Interface::Stream if !has_web_socket_stream => {
                return Err(io::Error::new(ErrorKind::Unsupported, "WebSocketStream not supported"))
            }
            Interface::Standard if !has_web_socket => {
                return Err(io::Error::new(ErrorKind::Unsupported, "WebSocket not supported"))
            }
            _ => (),
        }

        match interface {
            Interface::Stream => {
                let (stream, info) = stream::Inner::new(self).await?;
                Ok(WebSocket { inner: Inner::Stream(stream), info: Rc::new(info) })
            }
            Interface::Standard => {
                let (standard, info) = standard::Inner::new(self).await?;
                Ok(WebSocket { inner: Inner::Standard(standard), info: Rc::new(info) })
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
pub struct WebSocket {
    inner: Inner,
    info: Rc<Info>,
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
        let Self { inner, info } = self;
        match inner {
            Inner::Stream(inner) => {
                let (sender, receiver) = inner.into_split();
                let sender = WebSocketSender { inner: SenderInner::Stream(sender), info: info.clone() };
                let receiver = WebSocketReceiver { inner: ReceiverInner::Stream(receiver), info };
                (sender, receiver)
            }
            Inner::Standard(inner) => todo!(),
        }
    }

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), io::Error>> {
        match &mut self.inner {
            Inner::Stream(inner) => inner.sender.poll_ready_unpin(cx),
            Inner::Standard(inner) => todo!(),
        }
    }

    fn start_send(mut self: Pin<&mut Self>, item: &JsValue) -> Result<(), io::Error> {
        match &mut self.inner {
            Inner::Stream(inner) => inner.sender.start_send_unpin(item),
            Inner::Standard(inner) => todo!(),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), io::Error>> {
        match &mut self.inner {
            Inner::Stream(inner) => inner.sender.poll_flush_unpin(cx),
            Inner::Standard(inner) => todo!(),
        }
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), io::Error>> {
        match &mut self.inner {
            Inner::Stream(inner) => inner.sender.poll_close_unpin(cx),
            Inner::Standard(inner) => todo!(),
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

impl Stream for WebSocket {
    type Item = io::Result<Msg>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        match &mut self.inner {
            Inner::Stream(inner) => inner.receiver.poll_next_unpin(cx),
            Inner::Standard(inner) => todo!(),
        }
    }
}

/// Sending part of a [WebSocket].
pub struct WebSocketSender {
    inner: SenderInner,
    info: Rc<Info>,
}

enum SenderInner {
    Stream(stream::Sender),
    Standard(standard::ReaderInner),
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

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), io::Error>> {
        match &mut self.inner {
            SenderInner::Stream(inner) => inner.poll_ready_unpin(cx),
            SenderInner::Standard(inner) => todo!(),
        }
    }

    fn start_send(mut self: Pin<&mut Self>, item: &JsValue) -> Result<(), io::Error> {
        match &mut self.inner {
            SenderInner::Stream(inner) => inner.start_send_unpin(item),
            SenderInner::Standard(inner) => todo!(),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), io::Error>> {
        match &mut self.inner {
            SenderInner::Stream(inner) => inner.poll_flush_unpin(cx),
            SenderInner::Standard(inner) => todo!(),
        }
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), io::Error>> {
        match &mut self.inner {
            SenderInner::Stream(inner) => inner.poll_close_unpin(cx),
            SenderInner::Standard(inner) => todo!(),
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

/// Receiving part of a [WebSocket].
pub struct WebSocketReceiver {
    inner: ReceiverInner,
    info: Rc<Info>,
}

enum ReceiverInner {
    Stream(stream::Receiver),
    Standard(standard::ReaderInner),
}

impl fmt::Debug for WebSocketReceiver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
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
            ReceiverInner::Standard(inner) => todo!(),
        }
    }
}
