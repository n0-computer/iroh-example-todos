/// handlers for iroh::bytes connections
use std::sync::Arc;

use bytes::Bytes;
use futures::{future::BoxFuture, FutureExt};
use iroh::bytes::{
    protocol::{GetRequest, RequestToken},
    provider::{CustomGetHandler, EventSender, RequestAuthorizationHandler},
};
use iroh::{collection::IrohCollectionParser, database::flat::Database};

#[derive(Debug, Clone)]
pub struct IrohBytesHandlers {
    db: Database,
    rt: iroh::bytes::util::runtime::Handle,
    event_sender: NoopEventSender,
    get_handler: Arc<NoopCustomGetHandler>,
    auth_handler: Arc<NoopRequestAuthorizationHandler>,
}
impl IrohBytesHandlers {
    pub fn new(rt: iroh::bytes::util::runtime::Handle, db: Database) -> Self {
        Self {
            db,
            rt,
            event_sender: NoopEventSender,
            get_handler: Arc::new(NoopCustomGetHandler),
            auth_handler: Arc::new(NoopRequestAuthorizationHandler),
        }
    }
    pub async fn handle_connection(&self, conn: quinn::Connecting) -> anyhow::Result<()> {
        iroh::bytes::provider::handle_connection(
            conn,
            self.db.clone(),
            self.event_sender.clone(),
            IrohCollectionParser,
            self.get_handler.clone(),
            self.auth_handler.clone(),
            self.rt.clone(),
        )
        .await;
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct NoopEventSender;
impl EventSender for NoopEventSender {
    fn send(&self, _event: iroh::bytes::provider::Event) -> BoxFuture<()> {
        async {}.boxed()
    }
}
#[derive(Debug)]
struct NoopCustomGetHandler;
impl CustomGetHandler for NoopCustomGetHandler {
    fn handle(
        &self,
        _token: Option<RequestToken>,
        _request: Bytes,
    ) -> BoxFuture<'static, anyhow::Result<GetRequest>> {
        async move { Err(anyhow::anyhow!("no custom get handler defined")) }.boxed()
    }
}
#[derive(Debug)]
struct NoopRequestAuthorizationHandler;
impl RequestAuthorizationHandler for NoopRequestAuthorizationHandler {
    fn authorize(
        &self,
        token: Option<RequestToken>,
        _request: &iroh::bytes::protocol::Request,
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        async move {
            if let Some(token) = token {
                anyhow::bail!(
                    "no authorization handler defined, but token was provided: {:?}",
                    token
                );
            }
            Ok(())
        }
        .boxed()
    }
}
