use anyhow::{Context, Result};
use iroh::baomap::{flat::Store as BaoFileStore, mem::Store as BaoMemStore};
use iroh::node::Node;
use iroh_bytes::{baomap::Store as BaoStore, util::runtime};
use iroh_net::derp::DerpMap;
use iroh_net::tls::Keypair;
use iroh_sync::store::fs::Store as DocFileStore;
use iroh_sync::store::memory::Store as DocMemStore;
use iroh_sync::store::Store as DocStore;
use std::net::SocketAddr;
use std::path::PathBuf;

const DOCS_PATH: &str = "docs";

pub enum Iroh {
    FileStore(Node<BaoFileStore, DocFileStore>),
    MemStore(Node<BaoMemStore, DocMemStore>),
}

impl Iroh {
    pub async fn new(
        keypair: Keypair,
        derp_map: Option<DerpMap>,
        bind_addr: SocketAddr,
        data_root: Option<PathBuf>,
    ) -> Result<Self> {
        let rt = runtime::Handle::from_currrent(num_cpus::get())?;
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
    keypair: Keypair,
    derp_map: Option<DerpMap>,
    bind_addr: SocketAddr,
) -> Result<Node<BaoMemStore, DocMemStore>> {
    let rt_handle = rt.clone();
    create_iroh_node(
        BaoMemStore::new(rt),
        DocMemStore::default(),
        &rt_handle,
        keypair,
        derp_map,
        bind_addr,
    )
    .await
}

pub async fn create_iroh_node_file_store(
    rt: &runtime::Handle,
    keypair: Keypair,
    derp_map: Option<DerpMap>,
    bind_addr: SocketAddr,
    data_root: PathBuf,
) -> Result<Node<BaoFileStore, DocFileStore>> {
    let path = {
        if data_root.is_absolute() {
            data_root
        } else {
            std::env::current_dir()?.join(data_root)
        }
    };
    let bao_store = {
        tokio::fs::create_dir_all(&path).await?;
        BaoFileStore::load(&path, &path, &rt)
            .await
            .with_context(|| format!("Failed to load tasks database from {}", path.display()))?
    };
    let doc_store = {
        let path = path.join(DOCS_PATH);
        DocFileStore::new(path.clone()).with_context(|| {
            format!("Failed to load docs database from {:?}", path.display())
        })?
    };

    create_iroh_node(bao_store, doc_store, rt, keypair, derp_map, bind_addr).await
}

pub async fn create_iroh_node<B: BaoStore, D: DocStore>(
    bao_store: B,
    doc_store: D,
    rt: &runtime::Handle,
    keypair: Keypair,
    derp_map: Option<DerpMap>,
    bind_addr: SocketAddr,
) -> Result<Node<B, D>> {
    let mut builder = Node::builder(bao_store, doc_store);
    if let Some(dm) = derp_map {
        builder = builder.derp_map(dm);
    }
    builder
        .bind_addr(bind_addr)
        .runtime(rt)
        .keypair(keypair)
        .spawn()
        .await
}
