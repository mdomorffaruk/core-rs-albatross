use std::fmt;
use std::sync::Arc;

use futures::prelude::*;
use futures::sync::mpsc::*;
use parking_lot::Mutex;
use tokio::prelude::{Stream};

use crate::consensus::base::primitive::hash::Argon2dHash;
use crate::network::message::Message;
use crate::network::websocket::NimiqMessageStreamError;
use crate::network::websocket::SharedNimiqMessageStream;
use crate::network::peer::Peer;
use crate::network::connection::network_connection::AddressInfo;
use crate::utils::observer::Notifier;
use std::fmt::Debug;
use crate::network::connection::close_type::CloseType;
use crate::utils::observer::Listener;
use crate::network::connection::network_connection::NetworkConnection;
use parking_lot::RwLock;
use crate::utils::observer::ListenerHandle;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

#[derive(Debug)]
pub enum ProtocolError {
    SendError(SendError<Message>),
}

pub trait Agent: Send {
    /// Initialize the protocol.
    fn initialize(&mut self) {}

    /// Maintain the protocol state.
//    fn maintain(&mut self) {}

    /// Handle a message.
    fn on_message(&mut self, msg: &Message) -> Result<(), ProtocolError>;

    /// On disconnect.
    fn on_close(&mut self) {}

    /// Boxes the protocol.
    fn boxed(self) -> Box<Agent> where Self: Sized + 'static {
        Box::new(self)
    }
}

#[derive(Debug)]
pub struct PingAgent {
    sink: PeerSink,
}

impl PingAgent {
    pub fn new(sink: PeerSink) -> Self {
        PingAgent {
            sink,
        }
    }
}

impl Agent for PingAgent {
    fn on_message(&mut self, msg: &Message) -> Result<(), ProtocolError> {
        if let Message::Ping(nonce) = msg {
            // Respond with a pong message.
            self.sink.send(Message::Pong(*nonce))
                .map_err(|err| ProtocolError::SendError(err))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone)]
pub struct PeerChannel {
    stream_notifier: Arc<RwLock<Notifier<'static, PeerStreamEvent>>>,
    pub notifier: Arc<RwLock<Notifier<'static, PeerChannelEvent>>>,
    network_connection_listener_handle: ListenerHandle,
    peer_sink: PeerSink,
    pub address_info: AddressInfo,
    closed: Arc<AtomicBool>,
}

impl PeerChannel {
    pub fn new(network_connection: &NetworkConnection) -> Self {
        let notifier = Arc::new(RwLock::new(Notifier::new()));
        let closed = Arc::new(AtomicBool::new(false));
        let address_info = network_connection.address_info();

        let inner_closed = closed.clone();
        let bubble_notifier = notifier.clone();
        let network_connection_listener_handle = network_connection.notifier.write().register(move |e: &PeerStreamEvent| {
            let event = PeerChannelEvent::from(e);
            match event {
                PeerChannelEvent::Close(ty) => {
                    // Don't fire close event again when already closed.
                    if !inner_closed.swap(true, Ordering::Relaxed) {
                        bubble_notifier.read().notify(event);
                    }
                },
                event => bubble_notifier.read().notify(event),
            };
        });

        PeerChannel {
            stream_notifier: network_connection.notifier.clone(),
            notifier,
            network_connection_listener_handle,
            peer_sink: network_connection.peer_sink(),
            address_info,
            closed,
        }
    }

    pub fn closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }
}

impl Drop for PeerChannel {
    fn drop(&mut self) {
        self.stream_notifier.write().deregister(self.network_connection_listener_handle);
    }
}

impl Debug for PeerChannel {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(f, "PeerChannel {{}}")
    }
}

#[derive(Clone)]
pub enum PeerChannelEvent {
    Message(Arc<Message>),
    Close(CloseType),
    Error, // cannot use `NimiqMessageStreamError`, because `tungstenite::Error` is not `Clone`
}

impl<'a> From<&'a PeerStreamEvent> for PeerChannelEvent {
    fn from(e: &'a PeerStreamEvent) -> Self {
        match e {
            PeerStreamEvent::Message(msg) => PeerChannelEvent::Message(msg.clone()),
            PeerStreamEvent::Close(ty) => PeerChannelEvent::Close(*ty),
            PeerStreamEvent::Error => PeerChannelEvent::Error,
        }
    }
}

#[derive(Clone)]
pub struct PeerSink {
    sink: UnboundedSender<Message>,
}

impl PeerSink {
    pub fn new(channel: UnboundedSender<Message>) -> Self {
        PeerSink {
            sink: channel.clone(),
        }
    }

    pub fn send(&self, msg: Message) -> Result<(), SendError<Message>> {
        self.sink.unbounded_send(msg)
    }
}

impl Debug for PeerSink {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(f, "PeerSink {{}}")
    }
}

#[derive(Clone)]
pub enum PeerStreamEvent {
    Message(Arc<Message>),
    Close(CloseType),
    Error, // cannot use `NimiqMessageStreamError`, because `tungstenite::Error` is not `Clone`
}

pub struct PeerStream {
    stream: SharedNimiqMessageStream,
    notifier: Arc<RwLock<Notifier<'static, PeerStreamEvent>>>,
}

impl PeerStream {
    pub fn new(stream: SharedNimiqMessageStream, notifier: Arc<RwLock<Notifier<'static, PeerStreamEvent>>>) -> Self {
        PeerStream {
            stream,
            notifier,
        }
    }

    pub fn process_stream(self) -> impl Future<Item=(), Error=NimiqMessageStreamError> + 'static {
        let stream = self.stream;
        let msg_notifier = self.notifier.clone();
        let error_notifier = self.notifier.clone();
        let close_notifier = self.notifier;

        let process_message = stream.for_each(move |msg| {
            msg_notifier.read().notify(PeerStreamEvent::Message(Arc::new(msg)));
            Ok(())
        }).or_else(move |error| {
            error_notifier.read().notify(PeerStreamEvent::Error);
            Err(error)
        }).and_then(move |result| {
            close_notifier.read().notify(PeerStreamEvent::Close(CloseType::ClosedByRemote));
            Ok(result)
        });

        process_message
    }
}

impl Debug for PeerStream {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        self.stream.fmt(f)
    }
}
