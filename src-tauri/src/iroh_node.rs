use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};
use iroh::{
    bytes::{
        store::flat::Store as FileStore, store::mem::Store as MemStore, store::Store, util::runtime,
    },
    net::{derp::DerpMap, key::SecretKey},
    node::Node,
    sync::store::Store as DocStore,
};

pub enum Iroh {
    FileStore(Node<FileStore>),
    MemStore(Node<MemStore>),
}

impl Iroh {
    pub async fn new(
        keypair: SecretKey,
        derp_map: Option<DerpMap>,
        bind_addr: SocketAddr,
        data_root: Option<PathBuf>,
    ) -> Result<Self> {
        let rt = runtime::Handle::from_current(num_cpus::get())?;
        match data_root {
            Some(path) => Ok(Iroh::FileStore(
                create_iroh_node_file_store(&rt, keypair, derp_map, bind_addr, path).await?,
            )),
            None => Ok(Iroh::MemStore(
                create_iroh_node_mem_store(rt, keypair, derp_map, bind_addr).await?,
            )),
        }
    }

    pub fn client(&self) -> iroh::client::mem::Iroh {
        match self {
            Iroh::FileStore(node) => node.client(),
            Iroh::MemStore(node) => node.client(),
        }
    }

    pub fn shutdown(self) {
        match self {
            Iroh::FileStore(node) => node.shutdown(),
            Iroh::MemStore(node) => node.shutdown(),
        }
    }
}

pub async fn create_iroh_node_mem_store(
    rt: runtime::Handle,
    keypair: SecretKey,
    derp_map: Option<DerpMap>,
    bind_addr: SocketAddr,
) -> Result<Node<MemStore>> {
    let rt_handle = rt.clone();
    let doc_store = iroh::sync::store::memory::Store::default();
    create_iroh_node(
        MemStore::new(rt),
        doc_store,
        &rt_handle,
        keypair,
        derp_map,
        bind_addr,
    )
    .await
}

pub async fn create_iroh_node_file_store(
    rt: &runtime::Handle,
    keypair: SecretKey,
    derp_map: Option<DerpMap>,
    bind_addr: SocketAddr,
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
        FileStore::load(&path, &path, &path, &rt)
            .await
            .with_context(|| format!("Failed to load tasks database from {}", path.display()))?
    };

    let docs_path = path.join("docs.db");
    let docs = iroh::sync::store::fs::Store::new(&docs_path)?;

    create_iroh_node(store, docs, rt, keypair, derp_map, bind_addr).await
}

pub async fn create_iroh_node<S: Store, D: DocStore>(
    blobs_store: S,
    docs_store: D,
    rt: &runtime::Handle,
    secret_key: SecretKey,
    _derp_map: Option<DerpMap>,
    bind_addr: SocketAddr,
) -> Result<Node<S>> {
    let builder = Node::builder(blobs_store, docs_store);
    // if let Some(dm) = derp_map {
    //     builder = builder.derp_map(dm);
    // }
    builder
        .bind_addr(bind_addr)
        .runtime(rt)
        .secret_key(secret_key)
        .spawn()
        .await
}
