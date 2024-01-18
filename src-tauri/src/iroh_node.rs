use std::path::PathBuf;

use anyhow::{Context, Result};
use iroh::{
    bytes::{store::flat::Store as FileStore, store::mem::Store as MemStore, store::Store},
    net::key::SecretKey,
    node::Node,
    sync::store::Store as DocStore,
};
use tokio_util::task::LocalPoolHandle;

pub enum Iroh {
    FileStore(Node<FileStore>),
    MemStore(Node<MemStore>),
}

impl Iroh {
    pub async fn new(data_root: Option<PathBuf>) -> Result<Self> {
        // TODO: persist
        let keypair = SecretKey::generate();
        let rt = LocalPoolHandle::new(1);

        match data_root {
            Some(path) => Ok(Iroh::FileStore(
                create_iroh_node_file_store(&rt, keypair, path).await?,
            )),
            None => Ok(Iroh::MemStore(
                create_iroh_node_mem_store(&rt, keypair).await?,
            )),
        }
    }

    pub fn client(&self) -> iroh::client::mem::Iroh {
        match self {
            Iroh::FileStore(node) => node.client(),
            Iroh::MemStore(node) => node.client(),
        }
    }
}

async fn create_iroh_node_mem_store(
    rt: &LocalPoolHandle,
    keypair: SecretKey,
) -> Result<Node<MemStore>> {
    let doc_store = iroh::sync::store::memory::Store::default();
    create_iroh_node(MemStore::new(), doc_store, rt, keypair).await
}

async fn create_iroh_node_file_store(
    rt: &LocalPoolHandle,
    keypair: SecretKey,
    data_root: PathBuf,
) -> Result<Node<FileStore>> {
    let path = {
        if data_root.is_absolute() {
            data_root
        } else {
            std::env::current_dir()?.join(data_root)
        }
    };
    let store = {
        tokio::fs::create_dir_all(&path).await?;
        FileStore::load(&path)
            .await
            .with_context(|| format!("Failed to load tasks database from {}", path.display()))?
    };

    let docs_path = path.join("docs.db");
    let docs = iroh::sync::store::fs::Store::new(&docs_path)?;

    create_iroh_node(store, docs, rt, keypair).await
}

async fn create_iroh_node<S: Store, D: DocStore>(
    blobs_store: S,
    docs_store: D,
    rt: &LocalPoolHandle,
    secret_key: SecretKey,
) -> Result<Node<S>> {
    Node::builder(blobs_store, docs_store)
        .local_pool(rt)
        .secret_key(secret_key)
        .spawn()
        .await
}
