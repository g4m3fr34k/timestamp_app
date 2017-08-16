use futures::sync::mpsc;
use futures::{Future, Stream, Sink, Poll, Async};
use futures::stream::Fuse;
use tokio_core::reactor::Handle;

use std::time::{SystemTime};
use std::net::SocketAddr;

use events::Channel;
use node::{ExternalMessage, NodeTimeout};
use messages::RawMessage;

use super::error::{forget_result, log_error};
use super::network::{NetworkEvent, NetworkRequest};

pub trait SystemStateProvider: Send + Sync + 'static + ::std::fmt::Debug {
    fn listen_address(&self) -> SocketAddr;
    fn current_time(&self) -> SystemTime;
}

#[derive(Debug)]
pub enum Event {
    Network(NetworkEvent),
    Timeout(NodeTimeout),
    Api(ExternalMessage),
}

#[derive(Debug)]
pub struct TimeoutRequest(pub SystemTime, pub NodeTimeout);

#[derive(Debug)]
pub struct DefaultSystemState(pub SocketAddr);

impl SystemStateProvider for DefaultSystemState {
    fn listen_address(&self) -> SocketAddr { self.0 }
    fn current_time(&self) -> SystemTime { SystemTime::now() }
}

/// Channel for messages and timeouts.
#[derive(Debug, Clone)]
pub struct NodeSender {
    pub timeout: mpsc::Sender<TimeoutRequest>,
    pub network: mpsc::Sender<NetworkRequest>,
    pub external: mpsc::Sender<ExternalMessage>,
}

#[derive(Debug)]
pub struct NodeReceiver {
    pub timeout: mpsc::Receiver<TimeoutRequest>,
    pub network: mpsc::Receiver<NetworkRequest>,
    pub external: mpsc::Receiver<ExternalMessage>,
}

#[derive(Debug)]
pub struct NodeChannel(pub NodeSender, pub NodeReceiver);

impl NodeChannel {
    pub fn new(buffer: usize) -> NodeChannel {
        let (timeout_sender, timeout_receiver) = mpsc::channel(buffer);
        let (network_sender, network_receiver) = mpsc::channel(buffer);
        let (external_sender, external_receiver) = mpsc::channel(buffer);

        let sender = NodeSender {
            timeout: timeout_sender,
            network: network_sender,
            external: external_sender,
        };
        let receiver = NodeReceiver {
            timeout: timeout_receiver,
            network: network_receiver,
            external: external_receiver,
        };
        NodeChannel(sender, receiver)
    }
}

impl Channel for NodeSender {
    type ApplicationEvent = ExternalMessage;
    type Timeout = NodeTimeout;

    fn send_to(&mut self, handle: Handle, address: SocketAddr, message: RawMessage) {
        let request = NetworkRequest::SendMessage(address, message);
        let send_future = self.network
            .clone()
            .send(request)
            .map(forget_result)
            .map_err(log_error);
        handle.spawn(send_future);
    }

    fn add_timeout(&mut self, handle: Handle, timeout: Self::Timeout, time: SystemTime) {
        let request = TimeoutRequest(time, timeout);
        let send_future = self.timeout
            .clone()
            .send(request)
            .map(forget_result)
            .map_err(log_error);
        handle.spawn(send_future);
    }
}

#[derive(Debug)]
pub struct EventsAggregator<S1, S2, S3>
where
    S1: Stream,
    S2: Stream,
    S3: Stream,
{
    timeout: Fuse<S1>,
    network: Fuse<S2>,
    api: Fuse<S3>,
}

impl<S1, S2, S3> EventsAggregator<S1, S2, S3>
where
    S1: Stream,
    S2: Stream,
    S3: Stream,
{
    pub fn new(timeout: S1, network: S2, api: S3) -> EventsAggregator<S1, S2, S3> {
        EventsAggregator {
            network: network.fuse(),
            timeout: timeout.fuse(),
            api: api.fuse(),
        }
    }
}

impl<S1, S2, S3> Stream for EventsAggregator<S1, S2, S3>
where
    S1: Stream<Item = NodeTimeout>,
    S2: Stream<
        Item = NetworkEvent,
        Error = S1::Error,
    >,
    S3: Stream<
        Item = ExternalMessage,
        Error = S1::Error,
    >,
{
    type Item = Event;
    type Error = S1::Error;

    fn poll(&mut self) -> Poll<Option<Event>, Self::Error> {
        let mut stream_finished = false;
        // Check timeout events
        match self.timeout.poll()? {
            Async::Ready(Some(item)) => return Ok(Async::Ready(Some(Event::Timeout(item)))),
            // Just finish stream
            Async::Ready(None) => stream_finished = true,
            Async::NotReady => {}
        };
        // Check network events
        match self.network.poll()? {
            Async::Ready(Some(item)) => return Ok(Async::Ready(Some(Event::Network(item)))),
            // Just finish stream
            Async::Ready(None) => stream_finished = true,
            Async::NotReady => {}
        };
        // Check api events
        match self.api.poll()? {
            Async::Ready(Some(item)) => return Ok(Async::Ready(Some(Event::Api(item)))),
            // Just finish stream
            Async::Ready(None) => stream_finished = true,
            Async::NotReady => {}
        };

        Ok(if stream_finished {
            Async::Ready(None)
        } else {
            Async::NotReady
        })
    }
}

pub trait EventHandler {
    fn handle_event(&mut self, event: Event);
}