//! Provides an actor "handle", which enables communication with an actor.

use core::any::Any;

use crate::types::{Handle, errors::SendError, message::{MessageHandler, MessageSender, Handler, Message}, params::FluxionParams};
use crate::actor::Actor;
use alloc::boxed::Box;

/// # [`ActorHandle`]
/// A trait used when storing an actor handle in the system.
pub(crate) trait ActorHandle: Send + Sync + 'static {
    /// Returns this actor as an any type, allowing us to concretely downcast it.
    fn as_any(&self) -> &dyn Any;
}

/// # [`LocalHandle`]
/// This struct wraps a channel to communicate with an actor on the local system.

pub struct LocalHandle<C: FluxionParams, A: Actor<C>> {
    /// The channel that we wrap.
    pub(crate) sender: whisk::Channel<Box<dyn Handler<C, A>>>,
}

// Weird clone impl so that Actors do not have to be clonable.
impl<C: FluxionParams, A: Actor<C>> Clone for LocalHandle<C, A> {
    fn clone(&self) -> Self {
        Self { sender: self.sender.clone() }
    }
}

impl<C: FluxionParams, A: Actor<C>> LocalHandle<C, A> {

    /// Sends a message to the actor and waits for a response
    /// 
    /// # Errors
    /// Returns an error if no response is received
    pub async fn request<M: Message>(&self, message: M) -> Result<M::Response, SendError>
    where
        A: Handle<C, M> {

        // Create the message handle
        let (mh, rx) = MessageHandler::new(message);

        // Send the handler
        self.sender.send(Box::new(mh)).await;

        // Wait for a response
        rx.await.or(Err(SendError::NoResponse))
    }
}

/// [`MessageSender<M>`] is implemented on [`LocalHandle<A>`] for every message for which `A`
/// implements [`Handle`]
#[cfg_attr(async_trait, async_trait::async_trait)]
impl<C: FluxionParams, A: Handle<C, M>, M: Message> MessageSender<M> for LocalHandle<C, A> {
    async fn request(&self, message: M) -> Result<<M>::Response, SendError> {
        self.request(message).await
    }
}

impl<C: FluxionParams, A: Actor<C>> ActorHandle for LocalHandle<C, A> {
    fn as_any(&self) -> &dyn Any {
        self
    }
}