use std::sync::Arc;
use std::{collections::HashMap, fmt, str::FromStr};

use anyhow::{bail, Result};
use bytes::Bytes;
use iroh::net::{
    defaults::default_derp_map, magic_endpoint::get_alpn, tls::Keypair, MagicEndpoint,
};
use iroh::sync::{
    BlobStore, Doc as SyncDoc, DocStore, DownloadMode, LiveSync, PeerSource, SYNC_ALPN,
};
use iroh_gossip::{
    net::{GossipHandle, GOSSIP_ALPN},
    proto::TopicId,
};
use iroh_sync::{
    store::{self, Store as _},
    sync::{Author, Namespace, OnInsertCallback, SignedEntry},
};
use once_cell::sync::OnceCell;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

type Doc = SyncDoc<store::fs::Store>;

#[derive(Clone, Debug, Serialize, Deserialize)]
/// Task in a list of tasks
pub struct Task {
    /// Description of the task
    /// Limited to 2000 characters
    pub label: String,
    /// Record creation timestamp. Counted as micros since the Unix epoch.
    pub created: u64,
    /// Whether or not the task has been completed. Done tasks will show up in the task list until
    /// they are archived.
    pub done: bool,
    /// Indicates whether or not the task is tombstoned
    pub is_delete: bool,
    /// String id
    pub id: String,
}

const MAX_TASK_SIZE: usize = 2 * 1024;
const MAX_LABEL_LEN: usize = 2 * 1000;

impl Task {
    fn from_bytes(bytes: Bytes) -> Result<Self> {
        let task = postcard::from_bytes(&bytes)?;
        Ok(task)
    }

    fn as_bytes(self) -> Result<Bytes> {
        let mut buf = bytes::BytesMut::zeroed(MAX_TASK_SIZE);
        postcard::to_slice(&self, &mut buf)?;
        Ok(buf.freeze())
    }

    fn missing_task(id: String) -> Self {
        Self {
            label: String::from("Missing Content"),
            created: 0,
            done: false,
            is_delete: false,
            id,
        }
    }
}

/// List of tasks, including completed tasks that have not been archived
pub struct Tasks {
    doc: Doc,
    store: DocStore,
    ticket: Ticket,
    live_sync: LiveSync<store::fs::Store>,
    blob_store: BlobStore,
}

impl Tasks {
    pub async fn new(on_insert: Option<OnInsertCallback>) -> Result<Self> {
        // parse or generate our keypair
        // let keypair = match args.private_key {
        //     None => Keypair::generate(),
        //     Some(key) => parse_keypair(&key)?,
        // };
        let keypair = Keypair::generate();

        // configure our derp map
        // let derp_map = match (args.no_derp, args.derp) {
        //     (false, None) => Some(default_derp_map()),
        //     (false, Some(url)) => Some(derp_map_from_url(url)?),
        //     (true, None) => None,
        //     (true, Some(_)) => bail!("You cannot set --no-derp and --derp at the same time"),
        // };
        let derp_map = Some(default_derp_map());

        let bind_port = 0;

        // build our magic endpoint and the gossip protocol
        let (endpoint, gossip, initial_endpoints) = {
            // init a cell that will hold our gossip handle to be used in endpoint callbacks
            let gossip_cell: OnceCell<GossipHandle> = OnceCell::new();
            // init a channel that will emit once the initial endpoints of our local node are discovered
            let (initial_endpoints_tx, mut initial_endpoints_rx) = mpsc::channel(1);
            // build the magic endpoint
            let endpoint = MagicEndpoint::builder()
                .keypair(keypair.clone())
                .alpns(vec![
                    GOSSIP_ALPN.to_vec(),
                    SYNC_ALPN.to_vec(),
                    iroh::bytes::protocol::ALPN.to_vec(),
                ])
                .derp_map(derp_map)
                .on_endpoints({
                    let gossip_cell = gossip_cell.clone();
                    Box::new(move |endpoints| {
                        // send our updated endpoints to the gossip protocol to be sent as PeerData to peers
                        if let Some(gossip) = gossip_cell.get() {
                            gossip.update_endpoints(endpoints).ok();
                        }
                        // trigger oneshot on the first endpoint update
                        initial_endpoints_tx.try_send(endpoints.to_vec()).ok();
                    })
                })
                .bind(bind_port)
                .await?;

            // initialize the gossip protocol
            let gossip = GossipHandle::from_endpoint(endpoint.clone(), Default::default());
            // insert into the gossip cell to be used in the endpoint callbacks above
            gossip_cell.set(gossip.clone()).unwrap();

            // wait for a first endpoint update so that we know about at least one of our addrs
            let initial_endpoints = initial_endpoints_rx.recv().await.unwrap();
            // pass our initial endpoints to the gossip protocol so that they can be announced to peers
            gossip.update_endpoints(&initial_endpoints)?;
            (endpoint, gossip, initial_endpoints)
        };

        // let (topic, peers) = match &args.command {
        //     Command::Open { doc_name } => {
        //         let topic: TopicId = blake3::hash(doc_name.as_bytes()).into();
        //         println!(
        //             "> opening document {doc_name} as namespace {} and waiting for peers to join us...",
        //             fmt_hash(topic.as_bytes())
        //         );
        //         (topic, vec![])
        //     }
        //     Command::Join { ticket } => {
        //         let Ticket { topic, peers } = Ticket::from_str(ticket)?;
        //         println!("> joining topic {topic} and connecting to {peers:?}",);
        //         (topic, peers)
        //     }
        // };

        // TODO: allow opening an existing one
        let doc_name = "tasks-01";
        let topic: TopicId = blake3::hash(doc_name.as_bytes()).into();
        let peers = Vec::new();

        let our_ticket = {
            // add our local endpoints to the ticket and print it for others to join
            let addrs = initial_endpoints.iter().map(|ep| ep.addr).collect();
            let mut peers = peers.clone();
            peers.push(PeerSource {
                peer_id: endpoint.peer_id(),
                addrs,
                derp_region: endpoint.my_derp().await,
            });
            Ticket { peers, topic }
        };

        // unwrap our storage path or default to temp
        // TODO: use tauri folder?
        let storage_path = {
            let name = format!("iroh-todo-{}", endpoint.peer_id());
            let dir = std::env::temp_dir().join(name);
            if !dir.exists() {
                std::fs::create_dir(&dir).expect("failed to create temp dir");
            }
            dir
        };

        // create a runtime that can spawn tasks on a local-thread executors (to support !Send futures)
        let rt = iroh::bytes::util::runtime::Handle::from_currrent(num_cpus::get())?;

        // create a blob store (with a iroh-bytes database inside)
        let blobs =
            BlobStore::new(rt.clone(), storage_path.join("blobs"), endpoint.clone()).await?;

        // create a doc store for the iroh-sync docs
        let author = Author::from(keypair.secret().clone());
        let docs_path = storage_path.join("docs");
        tokio::fs::create_dir_all(&docs_path).await?;
        let docs = DocStore::new(blobs.clone(), author, docs_path)?;

        // create the live syncer
        let live_sync = LiveSync::<store::fs::Store>::spawn(endpoint.clone(), gossip.clone());

        // construct the state that is passed to the endpoint loop and from there cloned
        // into to the connection handler task for incoming connections.
        let state = Arc::new(State {
            gossip: gossip.clone(),
            docs: docs.clone(),
            bytes: super::IrohBytesHandlers::new(rt.clone(), blobs.db().clone()),
        });

        // spawn our endpoint loop that forwards incoming connections
        tokio::spawn(endpoint_loop(endpoint.clone(), state));

        // open our document and add to the live syncer
        let namespace = Namespace::from_bytes(topic.as_bytes());
        let doc = docs.create_or_open(namespace, DownloadMode::Always).await?;
        live_sync.add(doc.replica().clone(), peers.clone()).await?;

        if let Some(on_insert) = on_insert {
            doc.on_insert(on_insert);
        }
        Ok(Tasks {
            doc,
            store: docs,
            ticket: our_ticket,
            live_sync,
            blob_store: blobs,
        })
    }

    pub async fn shutdown(&self) -> Result<()> {
        // exit: cancel the sync and store blob database and document
        self.live_sync.cancel().await?;
        self.blob_store.save().await?;
        Ok(())
    }

    pub fn ticket(&self) -> String {
        self.ticket.to_string()
    }

    pub async fn add(&mut self, id: String, label: String) -> Result<()> {
        if label.len() > MAX_LABEL_LEN {
            bail!("label is too long, max size is {MAX_LABEL_LEN} characters");
        }
        let created = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .expect("time drift")
            .as_secs();
        let task = Task {
            label,
            created,
            done: false,
            is_delete: false,
            id: id.clone(),
        };
        self.insert_bytes(id.as_bytes(), task.as_bytes()?).await
    }
    pub async fn toggle_done(&mut self, id: String) -> Result<()> {
        let mut task = self.get_task(id.clone()).await?;
        task.done = !task.done;
        self.update_task(id.as_bytes(), task).await
    }

    pub async fn delete(&mut self, id: String) -> Result<()> {
        println!("delete {id}");
        let mut task = self.get_task(id.clone()).await?;
        task.is_delete = true;
        self.update_task(id.as_bytes(), task).await
    }

    pub async fn get_tasks(&self) -> Result<Vec<Task>> {
        let entries = self
            .store
            .store()
            .get_latest(self.doc.replica().namespace())?;
        let mut hash_entries: HashMap<Vec<u8>, SignedEntry> = HashMap::new();

        // only get most recent entry for the key
        // wish this had an easier api -> get_latest_for_each_key?
        for entry in entries {
            let (id, entry) = entry?;
            if let Some(other_entry) = hash_entries.get(id.key()) {
                let other_timestamp = other_entry.entry().record().timestamp();
                let this_timestamp = entry.entry().record().timestamp();
                if this_timestamp > other_timestamp {
                    hash_entries.insert(id.key().to_owned(), entry);
                }
            } else {
                hash_entries.insert(id.key().to_owned(), entry);
            }
        }
        let entries: Vec<_> = hash_entries.values().collect();
        let mut tasks = Vec::new();
        for entry in entries {
            let task = self.task_from_entry(entry).await?;
            if !task.is_delete {
                tasks.push(task);
            }
        }
        tasks.sort_by_key(|t| t.created);
        Ok(tasks)
    }

    async fn insert_bytes(&self, key: impl AsRef<[u8]>, content: Bytes) -> Result<()> {
        self.doc.insert_bytes(key, content).await?;
        Ok(())
    }

    async fn update_task(&mut self, key: impl AsRef<[u8]>, task: Task) -> Result<()> {
        let content = task.as_bytes()?;
        self.insert_bytes(key, content).await
    }

    async fn get_task(&self, id: String) -> Result<Task> {
        match self
            .store
            .store()
            .get_latest_by_key(self.doc.replica().namespace(), id.as_bytes())?
            .next()
        {
            Some(entry) => {
                let (_, entry) = entry?;
                self.task_from_entry(&entry).await
            }
            None => {
                bail!("key not found")
            }
        }
    }

    async fn task_from_entry(&self, entry: &SignedEntry) -> Result<Task> {
        let id = String::from_utf8(entry.entry().id().key().to_owned())?;
        match self.doc.get_content_bytes(entry).await {
            Some(b) => Task::from_bytes(b),
            None => Ok(Task::missing_task(id.clone())),
        }
    }
}

/// TODO: make actual error
#[derive(Debug)]
pub enum UpdateError {
    NoMoreUpdates,
    GetTasksError,
}

#[derive(Debug, Serialize, Deserialize)]
struct Ticket {
    topic: TopicId,
    peers: Vec<PeerSource>,
}
impl Ticket {
    /// Deserializes from bytes.
    fn from_bytes(bytes: &[u8]) -> anyhow::Result<Self> {
        postcard::from_bytes(bytes).map_err(Into::into)
    }
    /// Serializes to bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("postcard::to_stdvec is infallible")
    }
}

/// Serializes to base32.
impl fmt::Display for Ticket {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let encoded = self.to_bytes();
        let mut text = data_encoding::BASE32_NOPAD.encode(&encoded);
        text.make_ascii_lowercase();
        write!(f, "{text}")
    }
}

/// Deserializes from base32.
impl FromStr for Ticket {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = data_encoding::BASE32_NOPAD.decode(s.to_ascii_uppercase().as_bytes())?;
        let slf = Self::from_bytes(&bytes)?;
        Ok(slf)
    }
}

#[derive(Debug)]
struct State {
    gossip: GossipHandle,
    docs: DocStore,
    bytes: super::IrohBytesHandlers,
}

async fn endpoint_loop(endpoint: MagicEndpoint, state: Arc<State>) -> anyhow::Result<()> {
    while let Some(conn) = endpoint.accept().await {
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_connection(conn, state).await {
                println!("> connection closed, reason: {err}");
            }
        });
    }
    Ok(())
}

async fn handle_connection(mut conn: quinn::Connecting, state: Arc<State>) -> anyhow::Result<()> {
    let alpn = get_alpn(&mut conn).await?;
    println!("> incoming connection with alpn {alpn}");
    match alpn.as_bytes() {
        GOSSIP_ALPN => state.gossip.handle_connection(conn.await?).await,
        SYNC_ALPN => state.docs.handle_connection(conn).await,
        alpn if alpn == iroh::bytes::protocol::ALPN => state.bytes.handle_connection(conn).await,
        _ => bail!("ignoring connection: unsupported ALPN protocol"),
    }
}
