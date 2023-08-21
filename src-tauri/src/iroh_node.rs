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
const TASKS_DIR: &str = "tasks";

pub enum Iroh {
    FileStore(Node<BaoFileStore, DocFileStore>),
    MemStore(Node<BaoMemStore, DocMemStore>),
}

impl Iroh {
    pub async fn new(
        keypair: Keypair,
        derp_map: Option<DerpMap>,
        bind_addr: SocketAddr,
    ) -> Result<Self> {
        let rt = runtime::Handle::from_currrent(num_cpus::get())?;
        match std::env::var_os("TASKS_MEM_STORE") {
            None => Ok(Iroh::FileStore(
                create_iroh_node_file_store(&rt, keypair, derp_map, bind_addr).await?,
            )),
            Some(_) => Ok(Iroh::MemStore(
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
) -> Result<Node<BaoFileStore, DocFileStore>> {
    let path = tasks_iroh_config_root()?;
    println!("loading iroh tasks store from {path:?}");
    let bao_store = {
        tokio::fs::create_dir_all(&path).await?;
        BaoFileStore::load(&path, &path, &rt)
            .await
            .with_context(|| format!("Failed to load tasks database from {}", path.display()))?
    };
    let doc_store = {
        let path = path.join(DOCS_PATH);
        DocFileStore::new(path.clone())
            .with_context(|| format!("Failed to load docs database from {:?}", path.display()))?
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

fn tasks_iroh_config_root() -> anyhow::Result<PathBuf> {
    if let Some(val) = std::env::var_os("TASK_CONFIG_DIR") {
        return Ok(PathBuf::from(val));
    }
    let cfg = dirs_next::config_dir().ok_or_else(|| {
        anyhow::anyhow!("operating environment provides no directory for configuration")
    })?;
    Ok(cfg.join(TASKS_DIR))
}
