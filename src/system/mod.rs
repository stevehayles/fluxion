//! The implementation of systems and surrounding types

use std::{collections::HashMap, sync::Arc};

use tokio::sync::{broadcast, RwLock};

#[cfg(feature = "foreign")]
use tokio::sync::{mpsc, Mutex};

use crate::{
    actor::{
        handle::local::LocalHandle,
        supervisor::{ActorSupervisor, SupervisorErrorPolicy},
        Actor, ActorEntry,
    },
    error::SystemError,
    message::{
        handler::{HandleFederated, HandleMessage, HandleNotification},
        Message, Notification, DefaultNotification, DefaultFederated,
    },
};

#[cfg(feature="foreign")]
use crate::{
    actor::{
        ActorID,
        handle::{
            foreign::ForeignHandle,
            ActorHandle
        }
    },
    error::ActorError
};

#[cfg(feature="foreign")]
use crate::message::foreign::ForeignMessage;

#[cfg(any(not(feature="foreign"), not(feature="notifications")))]
use std::marker::PhantomData;


/// Internals used when foreign messages are enabled
#[cfg(feature = "foreign")]
mod foreign;

/// # ActorType
/// The type of an actor in the hashmap
#[cfg(feature = "foreign")]
type ActorType<F> = Box<dyn ActorEntry<Federated = F> + Send + Sync + 'static>;

#[cfg(not(feature = "foreign"))]
type ActorType<F> = (Box<dyn ActorEntry + Send + Sync + 'static>, PhantomData<F>);

/// # GetActorReturn
/// The return value of the `get_actor` function
#[cfg(feature = "foreign")]
pub(crate) type GetActorReturn<F, M> = Box<dyn ActorHandle<F, M>>;

#[cfg(not(feature = "foreign"))]
pub(crate) type GetActorReturn<F, M> = LocalHandle<F, M>;

/// # System
/// The core part of Fluxion, the [`System`] runs actors and handles communications between other systems.
///
/// ## Inter-System Communication
/// Fluxion systems enable communication by having what is called a foreign channel.
/// The foreign channel is an mpsc channel, the Reciever for which can be retrieved once by a single outside source using [`System::get_foreign`].
/// When a Message or Foreign Message is sent to an external actor, or a Notification is sent at all, the foreign
/// channel will be notified.
/// 
/// ## Using Clone
/// System uses [`Arc`] internally, so a [`System`] can be cloned where needed.
#[derive(Clone)]
pub struct System<F, N>
where
    F: Message,
{
    /// The id of the system
    id: String,

    /// Internals used when foreign messages are enabled
    #[cfg(feature = "foreign")]
    foreign: foreign::ForeignComponents<F, N>,

    /// Foreign messages are not enabled, the system needs a PhantomData to hold F
    #[cfg(not(feature = "foreign"))]
    _phantom: PhantomData<F>,

    /// A shutdown sender which tells all actors to stop
    shutdown: broadcast::Sender<()>,

    /// The hashmap of all actors
    actors: Arc<RwLock<HashMap<String, ActorType<F>>>>,

    /// The notification broadcast
    #[cfg(feature = "notifications")]
    notification: broadcast::Sender<N>,
    #[cfg(not(feature = "notifications"))]
    _notification: PhantomData<N>,
}

#[cfg(feature = "foreign")]
impl<F: Message, N> System<F, N> {
    /// Returns the foreign channel reciever wrapped in an [`Option<T>`].
    /// [`None`] will be returned if the foreign reciever has already been retrieved.
    pub async fn get_foreign(&self) -> Option<mpsc::Receiver<ForeignMessage<F>>> {
        // Lock the foreign reciever
        let mut foreign_reciever = self.foreign.foreign_reciever.lock().await;

        // Return the contents and replace with None
        std::mem::take(std::ops::DerefMut::deref_mut(&mut foreign_reciever))
    }

    /// Returns true if the given [`ActorPath`] is a foreign actor
    pub fn is_foreign(&self, actor: &crate::actor::path::ActorPath) -> bool {
        // If the first system in the actor exists and it it not this system, then it is a foreign system
        actor.first().is_some_and(|v| v != self.id)
    }

    /// Returns true if someone is waiting for a foreign message
    pub async fn can_send_foreign(&self) -> bool {
        self.foreign.foreign_reciever.lock().await.is_none()
    }

    /// Relays a foreign message to this system
    pub async fn relay_foreign(&self, foreign: ForeignMessage<F>) -> Result<(), ActorError> {
        // Get the target
        let target = foreign.get_target();

        // If it is a foreign actor or the lenth of the systems is larger than 1
        if self.is_foreign(target) || target.systems().len() > 1 {
            // Pop off the target if the top system is us (ex. "thissystem:foreign:actor")
            let foreign = if self.is_foreign(target) {
                foreign
            } else {
                foreign.pop_target()
            };

            // And relay
            self.foreign.foreign_sender
                .send(foreign)
                .await
                .or(Err(ActorError::ForeignSendFail))
        } else {
            // Send to a local actor
            self.send_foreign_to_local(foreign).await
        }
    }

    /// Sends a foreign message to a local actor
    #[cfg(feature = "federated")]
    async fn send_foreign_to_local(&self, foreign: ForeignMessage<F>) -> Result<(), ActorError> {
        match foreign {
            ForeignMessage::FederatedMessage(message, responder, target) => {
                // Get actors as read
                let actors = self.actors.read().await;

                // Get the local actor
                let actor = actors.get(target.actor());

                // If it does not exist, then error
                let Some(actor) = actor else {
                    return Err(ActorError::ForeignTargetNotFound);
                };

                // Send the message
                actor
                    .handle_foreign(ForeignMessage::FederatedMessage(message, responder, target))
                    .await
            }
            ForeignMessage::Message(message, responder, target) => {
                // Get actors as read
                let actors = self.actors.read().await;

                // Get the local actor
                let actor = actors.get(target.actor());

                // If it does not exist, then error
                let Some(actor) = actor else {
                    return Err(ActorError::ForeignTargetNotFound);
                };

                
                // Send the message
                actor
                    .handle_foreign(ForeignMessage::Message(message, responder, target))
                    .await
            }
        }
    }


    #[cfg(not(feature = "federated"))]
    async fn send_foreign_to_local(&self, foreign: ForeignMessage<F>) -> Result<(), ActorError> {
        // Get actors as read
        let actors = self.actors.read().await;

        // Get the local actor
        let actor = actors.get(foreign.target.actor());

        // If it does not exist, then error
        let Some(actor) = actor else {
            return Err(ActorError::ForeignTargetNotFound);
        };

        
        // Send the message
        actor
            .handle_foreign(foreign)
            .await
    }
}

#[cfg(feature = "foreign")]
#[cfg(feature = "notifications")]
impl<F: Message, N> System<F, N> {
    /// Subscribes to the foreign notification sender
    pub fn subscribe_foreign_notify(&self) -> broadcast::Receiver<N> {
        self.foreign.foreign_notification.subscribe()
    }

    /// Notifies only actors on this sytem
    pub fn notify_local(&self, notification: N) -> usize {
        self.notification.send(notification).unwrap_or(0)
    }

}

#[cfg(feature = "notifications")]
impl<F: Message, N: Clone> System<F, N> {
    /// Returns a notification reciever associated with the system's notification broadcaster.
    pub(crate) fn subscribe_notify(&self) -> broadcast::Receiver<N> {
        self.notification.subscribe()
    }

    /// Notifies all actors.
    /// Returns the number of actors notified on this sytem
    pub fn notify(&self, notification: N) -> usize {
        #[cfg(feature = "foreign")]
        let _ = self.foreign.foreign_notification.send(notification.clone());
        self.notification.send(notification).unwrap_or(0)
    }

    /// Yields the current task until all notifications have been recieved
    pub async fn drain_notify(&self) {
        while !self.notification.is_empty() {
            tokio::task::yield_now().await;
        }
    }
}



impl<F, N> System<F, N>
where
    F: Message,
    N: Notification,
{
    

    /// Gets the system's id
    pub fn get_id(&self) -> &str {
        &self.id
    }

    

    /// Adds an actor to the system
    pub async fn add_actor<
        A: Actor + HandleNotification<N> + HandleFederated<F> + HandleMessage<M>,
        M: Message,
    >(
        &self,
        actor: A,
        id: &str,
        error_policy: SupervisorErrorPolicy,
    ) -> Result<LocalHandle<F, M>, SystemError> {
        #[cfg(feature = "foreign")]
        let id = &(self.id.clone() + ":" + id);

        // Convert id to a path
        #[cfg(feature = "foreign")]
        let path = ActorID::new(id).ok_or(SystemError::InvalidPath)?;
        #[cfg(not(feature = "foreign"))]
        let path = id.to_string();

        // Lock write access to the actor map
        let mut actors = self.actors.write().await;

        // If the key is already in actors, return an error
        if actors.contains_key(id) {
            return Err(SystemError::ActorExists);
        }

        // Initialize the supervisor
        let (mut supervisor, handle) = ActorSupervisor::new(actor, path, self, error_policy);

        // Clone the system
        let system = self.clone();

        // Start the supervisor task
        tokio::spawn(async {
            // TODO: Log this.
            let _ = supervisor.run(system).await;
            let _ = supervisor.cleanup().await;

            drop(supervisor);
        });

        // Insert the handle into the map
        #[cfg(feature = "foreign")]
        let v: Box<dyn ActorEntry<Federated = F> + Send + Sync> = Box::new(handle.clone());
        #[cfg(not(feature = "foreign"))]
        let v: Box<dyn ActorEntry + Send + Sync> = Box::new(handle.clone());
        
        // If foreign messages are disabled, we need to fit some phantom data in here
        #[cfg(not(feature = "foreign"))]
        let v = (v, PhantomData::default());

        actors.insert(id.to_string(), v);

        // Return the handle
        Ok(handle)
    }

    /// Retrieves an actor from the system, returning None if the actor does not exist
    pub async fn get_actor<M: Message>(&self, id: &str) -> Option<GetActorReturn<F, M>> {
        // Get the actor path
        #[cfg(feature = "foreign")]
        let path = ActorID::new(id)?;
        #[cfg(not(feature = "foreign"))]
        let path = id.to_string();
        

        // If the first system exists and it is not this system, then create a foreign actor handle
        #[cfg(feature = "foreign")]
        if path.first().unwrap_or(&self.id) != self.id {
            // Create and return a foreign handle
            return Some(Box::new(ForeignHandle {
                foreign: self.foreign.foreign_sender.clone(),
                system: self.clone(),
                path,
            }));
        }

        // Lock read access to the actor map
        let actors = self.actors.read().await;


        // Try to get the actor
        #[cfg(feature = "foreign")]
        let actor = actors.get(path.actor())?;

        #[cfg(not(feature = "foreign"))]
        let actor = actors.get(&path)?;

        // If foreign messages are not enabled, get actual actor, not the PhantomData
        #[cfg(not(feature = "foreign"))]
        let actor = &actor.0;

        // Downcast and clone into a box.

        match actor.as_any().downcast_ref::<LocalHandle<F, M>>() {
            Some(v) => {
                let v = v.clone();

                #[cfg(feature = "foreign")]
                let v = Box::new(v);

                Some(v)
            },
            None => None,
        }
    }

    

    
    /// Shutsdown all actors on the system, drains the shutdown channel, and then clears all actors
    ///
    /// # Note
    /// This does not shutdown the system itself, but only actors running on the system. The system is cleaned up at drop.
    /// Even after calling this function, actors can still be added to the system. This returns the number of actors shutdown.
    pub async fn shutdown(&self) -> usize {
        // Send the shutdown signal
        let res = self.shutdown.send(()).unwrap_or(0);

        // Borrow the actor map as writable
        let mut actors = self.actors.write().await;

        // Clear the list
        actors.clear();
        actors.shrink_to_fit();

        // Remove the lock
        drop(actors);

        // Drain the shutdown list
        self.drain_shutdown().await;

        res
    }

    /// Subscribes to the shutdown reciever
    pub(crate) fn subscribe_shutdown(&self) -> broadcast::Receiver<()> {
        self.shutdown.subscribe()
    }

    /// Waits until all actors have shutdown
    async fn drain_shutdown(&self) {
        while !self.shutdown.is_empty() {
            tokio::task::yield_now().await;
        }
    }
}

/// Creates a new system with the given id and types for federated messages and notification.
/// Use this function when you are using both federated messages and notifications.
pub fn new<F: Message, N: Notification>(id: &str) -> System<F, N> {
    

    // Create the notification sender
    #[cfg(feature = "notifications")]
    let (notification, _) = broadcast::channel(64);

    

    // Create the shutdown sender
    let (shutdown, _) = broadcast::channel(8);

    // If the foreign feature is enabled, create the struct containing the foreign internals
    #[cfg(feature = "foreign")]
    let foreign = {
        // Create the foreign channel
        let (foreign_sender, foreign_reciever) = mpsc::channel(64);

        // Create the foreign notification sender
        let (foreign_notification, _) = broadcast::channel(64);

        foreign::ForeignComponents {
            foreign_notification,
            foreign_reciever: Arc::new(Mutex::new(Some(foreign_reciever))),
            foreign_sender
        }
    };

    System {
        id: id.to_string(),
        #[cfg(feature = "foreign")]
        foreign,
        #[cfg(not(feature="foreign"))]
        _phantom: PhantomData::default(),
        shutdown,
        actors: Default::default(),
        #[cfg(feature = "notifications")]
        notification,
        #[cfg(not(feature = "notifications"))]
        _notification: PhantomData::default(),
    }
}

/// Creates a new system that does not use federated messages or notifications
pub fn new_none(id: &str) -> System<DefaultFederated, DefaultNotification> {
    new(id)
}


/// Creates a new system that uses federated messages but not notifications.
pub fn new_federated<F: Message>(id: &str) -> System<F, DefaultNotification> {
    new(id)
}

/// Creates a new system that uses notifications but not federated messages
pub fn new_notifications<N: Notification>(id: &str) -> System<DefaultFederated, N> {
    new(id)
}