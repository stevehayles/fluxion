use fluxion::{message::{Message, handler::{HandleNotification, HandleMessage, HandleFederated}, Notification}, system::System, actor::{ Actor, supervisor::SupervisorErrorPolicy, context::ActorContext, path::ActorPath}, error:: ActorError};


#[derive(Clone, Debug)]
struct TestMessage;

impl Message for TestMessage {
    type Response = ();
}

#[derive(Clone, Debug)]
struct TestFederated;

impl Message for TestFederated {
    type Response = ();
}

struct TestActor;

#[async_trait::async_trait]
impl Actor for TestActor {
    /// Called upon actor initialization, when the supervisor begins to run.
    async fn initialize<F: Message, N: Notification>(&mut self, _context: &mut ActorContext<F, N>) -> Result<(), ActorError> {
        println!("initialize");
        Ok(())
    }

    /// Called upon actor deinitialization, when the supervisor stops.
    /// Note that this will not be called if the initialize function fails.
    /// For handling cases of initialization failure, use [`Actor::cleanup`]
    async fn deinitialize<F: Message, N: Notification>(&mut self, _context: &mut ActorContext<F, N>) -> Result<(), ActorError> {
        println!("deinitialize");
        Ok(())
    }


    /// Called when the actor supervisor is killed, either as the result of a graceful shutdown
    /// or if initialization fails.
    async fn cleanup(&mut self) -> Result<(), ActorError> {
        println!("cleanup");
        Ok(())
    }
}

#[async_trait::async_trait]
impl HandleNotification<()> for TestActor {
    async fn notified< F: Message>(&mut self, _context: &mut ActorContext<F, ()>, _notification: ()) -> Result<(), ActorError> {
        println!("notification");
        Ok(())
    }
}

#[async_trait::async_trait]
impl HandleMessage<TestMessage> for TestActor {
    async fn message<F: Message, N: Notification>(&mut self, _context: &mut ActorContext<F, N>, _message: TestMessage) -> Result<(), ActorError> {
        println!("message");
        Ok(())
    }
}

#[async_trait::async_trait]
impl HandleFederated<TestFederated> for TestActor {
    async fn federated_message<N: Notification>(&mut self, context: &mut ActorContext<TestFederated, N>, _message: TestFederated) -> Result<(), ActorError> {
        println!("federated message");
        let ar = context.get_actor::<TestMessage>("test").await.unwrap();
        ar.send(TestMessage).await.unwrap();
        Ok(())
    }
}

#[tokio::main]
async fn main() {
    let system = System::<TestFederated, ()>::new("host");

    system.add_actor::<TestActor, TestMessage>(TestActor, "test",  SupervisorErrorPolicy::default()).await.unwrap();
    let ah = system.get_actor("test").await.unwrap();
    
    ah.request(TestMessage).await.unwrap();
    ah.request_federated(TestFederated).await.unwrap();
    system.notify(()).await;
    system.drain_notify().await;
    system.relay_foreign(fluxion::message::foreign::ForeignMessage::Message(Box::new(TestMessage), None, ActorPath::new("test").unwrap())).await.unwrap();

    system.shutdown().await;
}