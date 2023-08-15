use std::{collections::HashMap, fmt, str::FromStr};

use anyhow::{bail, Result};
use bytes::Bytes;
use futures::{Stream, StreamExt};
use iroh::client::Doc;

use iroh::rpc_protocol::{DocTicket, ProviderRequest, ProviderResponse, ShareMode};
use iroh::sync::{LiveEvent, PeerSource};
use iroh_gossip::proto::TopicId;
use iroh_sync::store::{GetFilter, KeyFilter};
use iroh_sync::sync::{AuthorId, SignedEntry};
use quic_rpc::transport::flume::FlumeConnection;
use serde::{Deserialize, Serialize};

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

impl Task {
    fn from_bytes(bytes: Bytes) -> anyhow::Result<Self> {
        let task = postcard::from_bytes(&bytes)?;
        Ok(task)
    }

    fn as_bytes(self) -> anyhow::Result<Bytes> {
        let mut buf = bytes::BytesMut::zeroed(MAX_TASK_SIZE);
        postcard::to_slice(&self, &mut buf)?;
        Ok(buf.freeze())
    }

    fn as_vec(self) -> anyhow::Result<Vec<u8>> {
        let buf = self.as_bytes()?;
        Ok(buf[..].to_vec())
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

const MAX_TASK_SIZE: usize = 2 * 1024;
const MAX_LABEL_LEN: usize = 2 * 1000;

/// List of tasks, including completed tasks that have not been archived
pub struct Tasks {
    doc: Doc<FlumeConnection<ProviderResponse, ProviderRequest>>,
    iroh: iroh::client::mem::Iroh,
    ticket: DocTicket,
    author: AuthorId,
}

/// How to open the task list
pub enum TasksType {
    /// Create a new task list with the given name
    New(String),
    /// Open a new task list with the given name
    Open(String),
    /// Join an existing task list using the ticket
    Join(String),
}

/// Get a list of task list names from the iroh client
pub async fn get_task_lists_names(iroh: &iroh::client::mem::Iroh) -> anyhow::Result<Vec<String>> {
    Ok(get_task_lists_docs(iroh)
        .await?
        .into_iter()
        .map(|docs| docs.0)
        .collect())
}

/// Get a list of task list names & their associated docs from the iroh client
pub async fn get_task_lists_docs(
    iroh: &iroh::client::mem::Iroh,
) -> anyhow::Result<
    Vec<(
        String,
        Doc<FlumeConnection<ProviderResponse, ProviderRequest>>,
    )>,
> {
    let mut doc_ids = iroh.list_docs().await?;
    let mut docs = Vec::new();
    while let Some(id) = doc_ids.next().await {
        let id = id?;
        let doc = iroh.get_doc(id)?;
        let mut entries = doc
            .get(GetFilter {
                author: None,
                key: KeyFilter::Key(crate::tasks::TASK_LIST_NAME.to_vec()),
                latest: true,
            })
            .await?;
        let entry = entries.next().await.unwrap_or_else(|| {
            Err(anyhow::anyhow!(
                "fatal error: task list does not have a name"
            ))
        })?;
        let name = doc.get_content_bytes(&entry).await?;
        docs.push((String::from_utf8(name.to_vec())?, doc));
    }
    Ok(docs)
}

const TASK_LIST_NAME: &[u8] = b"metadata/task_list_name";
const TASK_PREFIX: &[u8] = b"task/";

impl Tasks {
    pub async fn new(typ: TasksType, iroh: iroh::client::mem::Iroh) -> anyhow::Result<Self> {
        let author = iroh.create_author().await?;

        let doc = match typ {
            TasksType::New(name) => {
                let docs = get_task_lists_names(&iroh).await?;
                if let Some(_) = docs.into_iter().find(|n| n == &name) {
                    bail!("task list with name {name} already exists");
                }
                let doc = iroh.create_doc().await?;
                doc.set_bytes(author, TASK_LIST_NAME.to_vec(), name.as_bytes().to_vec())
                    .await?;
                doc
            }
            TasksType::Open(name) => {
                let docs = get_task_lists_docs(&iroh).await?;
                match docs.into_iter().find(|d| d.0 == name) {
                    Some((_, doc)) => doc,
                    None => {
                        bail!("could not find task list {name}");
                    }
                }
            }
            TasksType::Join(ticket) => {
                let ticket = DocTicket::from_str(&ticket)?;
                iroh.import_doc(ticket).await?
            }
        };

        let ticket = doc.share(ShareMode::Write).await?;

        Ok(Tasks {
            author,
            doc,
            ticket,
            iroh,
        })
    }

    pub fn ticket(&self) -> String {
        self.ticket.to_string()
    }

    pub async fn doc_subscribe(&self) -> Result<impl Stream<Item = Result<LiveEvent>>> {
        self.doc.subscribe().await
    }

    pub async fn add(&mut self, id: String, label: String) -> anyhow::Result<()> {
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
        self.insert_task_bytes(id.as_bytes(), task.as_vec()?).await
    }
    pub async fn toggle_done(&mut self, id: String) -> anyhow::Result<()> {
        let mut task = self.get_task(id.clone()).await?;
        task.done = !task.done;
        self.update_task(id.as_bytes(), task).await
    }

    pub async fn delete(&mut self, id: String) -> anyhow::Result<()> {
        let mut task = self.get_task(id.clone()).await?;
        task.is_delete = true;
        self.update_task(id.as_bytes(), task).await
    }

    pub async fn update(&mut self, id: String, label: String) -> anyhow::Result<()> {
        if label.len() >= MAX_LABEL_LEN {
            bail!("label is too long, must be {MAX_LABEL_LEN} or shorter");
        }
        let mut task = self.get_task(id.clone()).await?;
        task.label = label;
        self.update_task(id.as_bytes(), task).await
    }

    pub async fn get_tasks(&self) -> anyhow::Result<Vec<Task>> {
        let mut entries = self
            .doc
            .get(GetFilter {
                latest: true,
                author: None,
                key: KeyFilter::Prefix(TASK_PREFIX.to_vec()),
            })
            .await?;
        let mut hash_entries: HashMap<Vec<u8>, SignedEntry> = HashMap::new();

        // only get most recent entry for the key
        // wish this had an easier api -> get_latest_for_each_key?
        while let Some(entry) = entries.next().await {
            let entry = entry?;
            let id = entry.entry().id();
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

    async fn insert_task_bytes(
        &self,
        key: impl AsRef<[u8]>,
        content: Vec<u8>,
    ) -> anyhow::Result<()> {
        self.doc
            .set_bytes(
                self.author,
                [TASK_PREFIX, key.as_ref()].concat().to_vec(),
                content,
            )
            .await?;
        Ok(())
    }

    async fn update_task(&mut self, key: impl AsRef<[u8]>, task: Task) -> anyhow::Result<()> {
        let content = task.as_vec()?;
        self.insert_task_bytes(key, content).await
    }

    async fn get_task(&self, id: String) -> anyhow::Result<Task> {
        let entry = self
            .doc
            .get_latest_by_key([TASK_PREFIX, id.as_bytes()].concat().to_vec())
            .await?;
        self.task_from_entry(&entry).await
    }

    async fn task_from_entry(&self, entry: &SignedEntry) -> anyhow::Result<Task> {
        let id = String::from_utf8(entry.entry().id().key().to_owned())?;
        match self.doc.get_content_bytes(entry).await {
            Ok(b) => Task::from_bytes(b),
            Err(_) => Ok(Task::missing_task(id[TASK_PREFIX.len()..].to_string())),
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
