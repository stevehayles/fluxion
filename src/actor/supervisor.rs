//! Contains `ActorSupervisor`, a struct containing a task that handles an actor's lifecycle.


use tokio::sync::{broadcast, mpsc};

#[cfg(feature = "tracing")]
use tracing::{event, Level};

use crate::{
    error::{policy::ErrorPolicy, ActorError},
    error_policy, handle_policy,
    message::{
        handler::{HandleFederated, HandleMessage, HandleNotification},
        AsMessageType, Message, MessageType, Notification, MT,
        
    },
    system::System,
};

#[cfg(feature = "foreign")]
use crate::message::{
    foreign::ForeignMessage,
    DualMessage,
};

#[cfg(any(feature = "foreign", feature = "federated"))]
use crate::message::LocalMessage;

#[cfg(not(feature = "notifications"))]
use std::marker::PhantomData;

use super::{context::ActorContext, handle::local::LocalHandle, ActorID, Actor};

/// # ActorSupervisor
/// [`ActorSupervisor`] handles an actor's lifecycle and the reciept of messages.
/// This is acheived by holding several recieve channels and running a task that [`tokio::select`]s on them.
pub struct ActorSupervisor<A, F: Message, N: Notification, M: Message> {
    /// The actor managed by this supervisor
    actor: A,

    /// This actor's id
    id: ActorID,

    /// The actor's error policy
    error_policy: SupervisorErrorPolicy,

    /// The notification reciever
    #[cfg(feature = "notifications")]
    notify: broadcast::Receiver<N>,
    #[cfg(not(feature = "notifications"))]
    _notify: PhantomData<N>,

    /// The message reciever
    message: mpsc::Receiver<MT<F, M>>,

    /// The shutdown reciever
    shutdown: broadcast::Receiver<()>,
}

impl<A, F, N, M> ActorSupervisor<A, F, N, M>
where
    A: Actor,
    F: Message,
    N: Notification,
    M: Message,
{
    /// Gets the supervisor's actor id
    pub fn get_id(&self) -> ActorID {
        self.id.clone()
    }

    /// Creates a new supervisor with the given actor and actor id.
    /// Returns the new supervisor alongside the handle that holds the message sender.
    #[cfg_attr(feature = "tracing", tracing::instrument(skip(actor, system, error_policy)))]
    pub fn new(
        actor: A,
        id: ActorID,
        system: &System<F, N>,
        error_policy: SupervisorErrorPolicy,
    ) -> (ActorSupervisor<A, F, N, M>, LocalHandle<F, M>) {

        #[cfg(all(feature = "tracing", debug_assertions))]
        event!(Level::TRACE, system=system.get_id(), actor=id.to_string(), "Creating new actor supervisor.");

        // Create a new message channel
        let (message_sender, message) = mpsc::channel(64);

        #[cfg(all(feature = "tracing", debug_assertions))]
        event!(Level::TRACE, system=system.get_id(), actor=id.to_string(), "Created supervisor message channel.");

        // Subscribe to the notification broadcaster
        #[cfg(feature = "notifications")]
        let notify = system.subscribe_notify();

        #[cfg(feature = "notifications")]
        #[cfg(all(feature = "tracing", debug_assertions))]
        event!(Level::TRACE, system=system.get_id(), actor=id.to_string(), "Created supervisor notification channel.");

        // Subscribe to the shutdown reciever
        let shutdown = system.subscribe_shutdown();

        #[cfg(all(feature = "tracing", debug_assertions))]
        event!(Level::TRACE, system=system.get_id(), actor=id.to_string(), "Created supervisor shutdown channel.");

        // Create the supervisor
        let supervisor = Self {
            actor,
            #[cfg(feature = "notifications")]
            notify,
            #[cfg(not(feature = "notifications"))]
            _notify: PhantomData::default(),
            message,
            shutdown,
            error_policy,
            id: id.clone(),
        };

        // Create the handle
        let handle = LocalHandle {
            message_sender,
            id,
        };

        #[cfg(all(feature = "tracing", debug_assertions))]
        event!(Level::TRACE, system=system.get_id(), actor=supervisor.id.to_string(), "Created supervisor and actor handle.");

        // Return both
        (supervisor, handle)
    }
}

impl<A, F, N, M> ActorSupervisor<A, F, N, M>
where
    A: Actor + HandleNotification<N> + HandleFederated<F> + HandleMessage<M>,
    F: Message,
    N: Notification,
    M: Message,
{

    /// Handles a notification.
    /// If notifications are not enabled, this will return immediately.
    #[cfg(feature = "notifications")]
    #[cfg_attr(feature = "tracing", tracing::instrument(skip(self, context, notification)))]
    async fn handle_notification(&mut self, context: &mut ActorContext<F, N>, notification: Result<Result<N, broadcast::error::RecvError>, broadcast::error::RecvError> ) -> Result<(), ActorError> {
        // If the policy failed, then exit the loop
        let Ok(notification) = notification else {
            #[cfg(feature = "tracing")]
            event!(Level::ERROR, supervisor=self.id.to_string(), "Error policy failed when handling notification on supervisor.");

            return Err(ActorError::NotificationError);
        };

        // If the policy succeeded, but we failed to recieve, then continue. Otherwise handle it.
        let Ok(notification) = notification else {
            #[cfg(feature = "tracing")]
            event!(Level::WARN, supervisor=self.id.to_string(), "Error policy succeeded, but we failed to recieve notification.");

            return Ok(());
        };

        #[cfg(all(feature = "tracing", debug_assertions))]
        event!(Level::TRACE, supervisor=self.id.to_string(), "Handling notification.");

        // Call the handler, handling error policy
        let _ = handle_policy!(
            self.actor.notified(context, notification.clone()).await,
            |_| &self.error_policy.notification_handler,
            (), ActorError).await?;
        
        Ok(())
    }

    

    /// Runs the actor, only returning an error after all error policy options have been exhausted.
    #[cfg_attr(feature = "tracing", tracing::instrument(skip(self, system)))]
    pub async fn run(&mut self, system: System<F, N>) -> Result<(), ActorError> {

        #[cfg(all(feature = "tracing", debug_assertions))]
        event!(Level::DEBUG, supervisor=self.id.to_string(), "Running actor supervisor.");

        // Create a new actor context for this actor to use
        let mut context = ActorContext {
            id: self.id.clone(),
            system,
        };

        #[cfg(all(feature = "tracing", debug_assertions))]
        event!(Level::DEBUG, supervisor=self.id.to_string(), "Initializing actor.");

        // Initialize the actor, following error policy.
        let _err = handle_policy!(
            self.actor.initialize(&mut context).await,
            |_| &self.error_policy.initialize,
            (),
            ActorError
        )
        .await?;

        #[cfg(feature = "tracing")]
        match _err {
            Ok(_) => if cfg!(debug_assertions) {
                 event!(Level::TRACE, supervisor=self.id.to_string(), "Sucessfully initialized actor.")
            } else { () },
            Err(e) => event!(Level::WARN, supervisor=self.id.to_string(), error=format!("{}", e), "Initializing actor failed, but error policy succeeded."),
        };

        // Begin main loop
        loop {
            // Select on recieving messages
            tokio::select! {
                _ = self.shutdown.recv() => {
                    // Just shutdown no matter what happened
                    #[cfg(all(feature = "tracing", debug_assertions))]
                    event!(Level::DEBUG, supervisor=self.id.to_string(), "Actor recieved shutdown signal. Exiting main loop.");
                    break;
                },
                
                notification = async {
                    #[cfg(not(feature = "notifications"))]
                    {
                        loop {
                            tokio::task::yield_now().await;
                        }
                        //Err(broadcast::error::RecvError::Closed)
                    }
                    #[cfg(feature = "notifications")]
                    {
                        handle_policy!(
                            self.notify.recv().await,
                            |e: &broadcast::error::RecvError | match e {
                                broadcast::error::RecvError::Closed => &self.error_policy.notification_channel_closed,
                                broadcast::error::RecvError::Lagged(_) => &self.error_policy.notification_channel_lagged,
                            },
                            N, broadcast::error::RecvError).await
                    }
                } => {
                    #[cfg(all(feature = "tracing", debug_assertions))]
                    event!(Level::DEBUG, supervisor=self.id.to_string(), "Handling a notification.");

                    // Prevent a warning
                    #[cfg(not(feature = "notifications"))]
                    #[allow(clippy::let_unit_value)]
                    let _ = notification;

                    #[cfg(feature = "notifications")]
                    let res = self.handle_notification(&mut context, notification).await;

                    #[cfg(feature = "notifications")]
                    if res.is_err() {
                        #[cfg(feature = "tracing")]
                        event!(Level::ERROR, supervisor=self.id.to_string(), "Actor failed to handle notification.");

                        break;
                    }
                },
                message = handle_policy!(
                    self.message.recv().await.ok_or(ActorError::MessageChannelClosed),
                    |_| &self.error_policy.message_channel_closed,
                    MT<F, M>, ActorError) => {


                    // If the policy failed, then exit the loop
                    let Ok(message) = message else {
                        #[cfg(feature = "tracing")]
                        if let Err(e) = message {
                            event!(Level::ERROR, supervisor=self.id.to_string(), error=format!("{}", e), "Actor failed to recieve message, error policy failed");
                        }
                        break;
                    };

                    // If the policy succeeded, but we failed to recieve, then continue. Otherwise handle it.
                    let Ok(message) = message else {
                        #[cfg(feature = "tracing")]
                        if let Err(e) = message {
                            event!(Level::WARN, supervisor=self.id.to_string(), error=format!("{}", e), "Actor failed to recieve message, but the error policy succeeded");
                        }
                        continue;
                    };

                    // Get the message, downcasting if foreign. This always does at least one clone, but it appears to be unavoidable.
                    let message_type = handle_policy!(
                        message.as_message_type(),
                        |_| &self.error_policy.unexpected_foreign,
                        MessageType<F, M>, ActorError).await;

                    // If the policy failed, exit
                    let Ok(message_type) = message_type else {
                        #[cfg(feature = "tracing")]
                        if let Err(e) = message_type {
                            event!(Level::ERROR, supervisor=self.id.to_string(), error=format!("{}", e), "Actor failed to convert message to proper type, error policy failed");
                        }
                        break;
                    };

                    // If the policy succeeded, but we failed to recieve, then continue.
                    let Ok(message_type) = message_type else {
                        #[cfg(feature = "tracing")]
                        if let Err(e) = message_type {
                            event!(Level::WARN, supervisor=self.id.to_string(), error=format!("{}", e), "Actor failed to convert message to proper type, but the error policy succeeded");
                        }
                        continue;
                    };

                    // Given the message type, call the proper handler
                    #[cfg(feature = "federated")]
                    match message_type {
                        MessageType::Federated(m) => {
                            #[cfg(all(feature = "tracing", debug_assertions))]
                            event!(Level::DEBUG, supervisor=self.id.to_string(), "Handling a federated message.");

                            let res = handle_policy!(
                                self.actor.federated_message(&mut context, m.clone()).await,
                                |_| &self.error_policy.federated_handler,
                                F::Response, ActorError).await;

                            // If the policy failed, exit
                            let Ok(res) = res else {
                                #[cfg(feature = "tracing")]
                                if let Err(e) = res {
                                    event!(Level::ERROR, supervisor=self.id.to_string(), error=format!("{}", e), "Actor failed to handle federated message, error policy failed");
                                }
                                break;
                            };

                            // If the policy succeeded, but we failed to recieve, then continue.
                            let Ok(res) = res else {
                                #[cfg(feature = "tracing")]
                                if let Err(e) = res {
                                    event!(Level::WARN, supervisor=self.id.to_string(), error=format!("{}", e), "Actor failed to handle federated message, but the error policy succeeded");
                                }
                                continue;
                            };

                            // Match on the responder
                            #[cfg(feature = "foreign")]
                            let responder = match message {
                                DualMessage::LocalMessage(LocalMessage::Federated(_, Some(responder))) => Some(responder),
                                DualMessage::ForeignMessage(ForeignMessage::FederatedMessage(_, Some(responder), _)) => Some(responder),
                                _ => None
                            };

                            // If foreign messages are not enabled, use if let to get the responder out of the message
                            #[cfg(not(feature = "foreign"))]
                            let responder = if let LocalMessage::Federated(_, responder) = message {
                                responder
                            } else {
                                None
                            };

                            // If we need to respond, do so
                            if let Some(responder) = responder {
                                #[cfg(all(feature = "tracing", debug_assertions))]
                                event!(Level::DEBUG, supervisor=self.id.to_string(), "Sending response to federated message");
                                // This is a oneshot, so ignore if error
                                let _ = responder.send(res);
                            }
                        },
                        MessageType::Message(m) => {
                            #[cfg(all(feature = "tracing", debug_assertions))]
                            event!(Level::DEBUG, supervisor=self.id.to_string(), "Handling a message.");

                            let res = handle_policy!(
                                self.actor.message(&mut context, m.clone()).await,
                                |_| &self.error_policy.message_handler,
                                M::Response, ActorError).await;

                            // If the policy failed, exit
                            let Ok(res) = res else {
                                #[cfg(feature = "tracing")]
                                if let Err(e) = res {
                                    event!(Level::ERROR, supervisor=self.id.to_string(), error=format!("{}", e), "Actor failed to handle message, error policy failed");
                                }
                                break;
                            };

                            // If the policy succeeded, but we failed to recieve, then continue.
                            let Ok(res) = res else {
                                #[cfg(feature = "tracing")]
                                if let Err(e) = res {
                                    event!(Level::WARN, supervisor=self.id.to_string(), error=format!("{}", e), "Actor failed to handle message, but the error policy succeeded");
                                }
                                continue;
                            };

                            // Match on the responder, and respond if found
                            // Use the if let if the foreign feature is not enabled.
                            #[cfg(feature = "foreign")]
                            match message {
                                DualMessage::LocalMessage(LocalMessage::Message(_, Some(responder))) => {
                                    // Just send the response, ignoring the error
                                    let _ = responder.send(res);
                                },
                                DualMessage::ForeignMessage(ForeignMessage::Message(_, Some(responder), _)) => {
                                    // Box and send the response
                                    #[cfg(not(feature="bincode"))]
                                    let _ = responder.send(Box::new(res));

                                    #[cfg(feature="bincode")]
                                    let _ = responder.send(bincode::serialize(&res).or(Err(ActorError::ForeignRespondFailed))?);
                                },
                                _ => {}
                            };

                            #[cfg(not(feature = "foreign"))]
                            if let LocalMessage::Message(_, Some(responder)) = message {
                                #[cfg(all(feature = "tracing", debug_assertions))]
                                event!(Level::DEBUG, supervisor=self.id.to_string(), "Sending response to message");
                                // Just send the response, ignoring the error
                                let _ = responder.send(res);
                            }
                        },
                    };

                    // Do the same thing if federated messages are not enabled.
                    #[cfg(not(feature = "federated"))]
                    {   
                        #[cfg(all(feature = "tracing", debug_assertions))]
                        event!(Level::DEBUG, supervisor=self.id.to_string(), "Handling a message.");
                        
                        let res = handle_policy!(
                            self.actor.message(&mut context, message_type.0.clone()).await,
                            |_| &self.error_policy.message_handler,
                            M::Response, ActorError).await;
    
                        // If the policy failed, exit
                        let Ok(res) = res else {
                            #[cfg(feature = "tracing")]
                            if let Err(e) = res {
                                event!(Level::ERROR, supervisor=self.id.to_string(), error=format!("{}", e), "Actor failed to handle message, error policy failed");
                            }
                            break;
                        };
    
                        // If the policy succeeded, but we failed to recieve, then continue.
                        let Ok(res) = res else {
                            #[cfg(feature = "tracing")]
                            if let Err(e) = res {
                                event!(Level::WARN, supervisor=self.id.to_string(), error=format!("{}", e), "Actor failed to handle message, but the error policy succeeded");
                            }
                            continue;
                        };
    
                        // Match on the responder, and respond if found
                        // Use the if let if the foreign feature is not enabled.
                        #[cfg(feature = "foreign")]
                        match message {
                            #[cfg(not(feature = "federated"))]
                            DualMessage::LocalMessage(LocalMessage(_, Some(responder), _)) => {
                                #[cfg(all(feature = "tracing", debug_assertions))]
                                event!(Level::DEBUG, supervisor=self.id.to_string(), "Sending response to message");
                                // Just send the response, ignoring the error
                                let _ = responder.send(res);
                            },
                            #[cfg(feature = "federated")]
                            DualMessage::LocalMessage(LocalMessage::Message(_, Some(responder))) => {
                                #[cfg(all(feature = "tracing", debug_assertions))]
                                event!(Level::DEBUG, supervisor=self.id.to_string(), "Sending response to message");
                                // Just send the response, ignoring the error
                                let _ = responder.send(res);
                            },
                            #[cfg(feature = "federated")]
                            DualMessage::ForeignMessage(ForeignMessage::Message(_, Some(responder), _)) => {
                                #[cfg(all(feature = "tracing", debug_assertions))]
                                event!(Level::DEBUG, supervisor=self.id.to_string(), "Sending response to message");
                                // Box and send the response
                                #[cfg(not(feature="bincode"))]
                                let _ = responder.send(Box::new(res));
    
                                #[cfg(feature="bincode")]
                                let _ = responder.send(bincode::serialize(&res).or(Err(ActorError::ForeignRespondFailed))?);
                            },
                            #[cfg(not(feature = "federated"))]
                            DualMessage::ForeignMessage(ForeignMessage { responder: Some(responder), .. }) => {
                                #[cfg(all(feature = "tracing", debug_assertions))]
                                event!(Level::DEBUG, supervisor=self.id.to_string(), "Sending response to message");
                                // Box and send the response
                                #[cfg(not(feature="bincode"))]
                                let _ = responder.send(Box::new(res));
    
                                #[cfg(feature="bincode")]
                                let _ = responder.send(bincode::serialize(&res).or(Err(ActorError::ForeignRespondFailed))?);
                            },
                            _ => {}
                        };
    
                        #[cfg(not(feature = "foreign"))]
                        if let Some(responder) = message.1 {
                            #[cfg(all(feature = "tracing", debug_assertions))]
                                event!(Level::DEBUG, supervisor=self.id.to_string(), "Sending response to message");
                            // Just send the response, ignoring the error
                            let _ = responder.send(res);
                        }
                    }
                }
            }
        }

        #[cfg(all(feature = "tracing", debug_assertions))]
        event!(Level::DEBUG, supervisor=self.id.to_string(), "Deinitializing actor.");

        // Deinitialize the actor, following error policy
        let _err = handle_policy!(
            self.actor.deinitialize(&mut context).await,
            |_| &self.error_policy.deinitialize,
            (),
            ActorError
        )
        .await?;
        

        #[cfg(feature = "tracing")]
        match _err {
            Ok(_) => if cfg!(debug_assertions) {
                event!(Level::TRACE, supervisor=self.id.to_string(), "Sucessfully deinitialized actor.")
           } else { () },
            Err(e) => event!(Level::WARN, supervisor=self.id.to_string(), error=format!("{}", e), "Denitializing actor failed, but error policy succeeded."),
        };

        Ok(())
    }

    /// Cleans up the actor
    #[cfg_attr(feature = "tracing", tracing::instrument(skip(self)))]
    pub async fn cleanup(&mut self) -> Result<(), ActorError> {
        
        #[cfg(all(feature = "tracing", debug_assertions))]
        event!(Level::DEBUG, supervisor=self.id.to_string(), "Cleaning up actor");
        
        // Close the message channel
        self.message.close();

        #[cfg(all(feature = "tracing", debug_assertions))]
        event!(Level::TRACE, supervisor=self.id.to_string(), "Closed message channel.");

        // Cleanup the actor, following error policy
        let res = handle_policy!(
            self.actor.cleanup().await,
            |_| &self.error_policy.cleanup,
            (),
            ActorError
        )
        .await;

        // If the policy failed, then log and fail.
        let res = match res {
            Ok(res) => res,
            Err(e) => {
                #[cfg(feature = "tracing")]
                event!(Level::ERROR, supervisor=self.id.to_string(), error=format!("{}", e), "Actor failed to cleanup, error policy failed");

                return Err(e);
            }
        };

        // If the policy succeeded, but we failed to recieve, then log.
        if let Err(_) = res {
            #[cfg(feature = "tracing")]
            if let Err(e) = res {
                event!(Level::WARN, supervisor=self.id.to_string(), error=format!("{}", e), "Actor failed to recieve message, but the error policy succeeded");
            }
        };
        
        Ok(())
    }
}

/// # SupervisorErrorPolicy
/// The error policies used by an actor supervisor.
#[derive(Clone, Debug)]
pub struct SupervisorErrorPolicy {
    /// Called when actor initialization fails
    pub initialize: ErrorPolicy<ActorError>,
    /// Called when actor deinitialization fails
    pub deinitialize: ErrorPolicy<ActorError>,
    /// Called when actor cleanup fails
    pub cleanup: ErrorPolicy<ActorError>,
    /// Called when an actor notification channel is dropped.
    /// This should *never* ignore, as it could cause an actor
    /// to be orphaned and run forever.
    pub notification_channel_closed: ErrorPolicy<broadcast::error::RecvError>,
    /// Called when a notification channel laggs
    pub notification_channel_lagged: ErrorPolicy<broadcast::error::RecvError>,
    /// Called when an actor's notification handler fails
    pub notification_handler: ErrorPolicy<ActorError>,
    /// Called when an actor's message channel closes
    pub message_channel_closed: ErrorPolicy<ActorError>,
    /// Called when a foreign message failed to downcast
    pub unexpected_foreign: ErrorPolicy<ActorError>,
    /// Called when an actor's federated message handler fails
    pub federated_handler: ErrorPolicy<ActorError>,
    /// Called when an actor's message handler fails
    pub message_handler: ErrorPolicy<ActorError>,
    /// Called when a federated message fails to send its response
    pub federated_respond: ErrorPolicy<ActorError>,
}

impl Default for SupervisorErrorPolicy {
    fn default() -> Self {
        Self {
            initialize: error_policy! {
                fail;
            },
            deinitialize: error_policy! {
                fail;
            },
            cleanup: error_policy! {
                fail;
            },
            notification_channel_closed: error_policy! {
                fail;
            },
            notification_channel_lagged: error_policy! {
                ignore;
            },
            notification_handler: error_policy! {
                ignore;
            },
            message_channel_closed: error_policy! {
                fail;
            },
            unexpected_foreign: error_policy! {
                ignore;
            },
            federated_handler: error_policy! {
                ignore;
            },
            message_handler: error_policy! {
                ignore;
            },
            federated_respond: error_policy! {
                ignore;
            },
        }
    }
}
