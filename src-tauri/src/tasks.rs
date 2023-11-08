use std::{collections::HashMap, str::FromStr};

use anyhow::{bail, Result};
use bytes::Bytes;
use futures::{Stream, StreamExt};
use iroh::client::{Doc, Iroh};
use iroh::rpc_protocol::{DocTicket, ProviderRequest, ProviderResponse, ShareMode};
use iroh::sync::{AuthorId, Entry};
use iroh::sync_engine::LiveEvent;
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
    node: Iroh<FlumeConnection<ProviderResponse, ProviderRequest>>,
    doc: Doc<FlumeConnection<ProviderResponse, ProviderRequest>>,
    ticket: DocTicket,
    author: AuthorId,
}

impl Tasks {
    pub async fn new(
        ticket: Option<String>,
        node: Iroh<FlumeConnection<ProviderResponse, ProviderRequest>>,
    ) -> anyhow::Result<Self> {
        let author = node.authors.create().await?;

        let doc = match ticket {
            None => node.docs.create().await?,
            Some(ticket) => {
                let ticket = DocTicket::from_str(&ticket)?;
                node.docs.import(ticket).await?
            }
        };

        let ticket = doc.share(ShareMode::Write).await?;

        Ok(Tasks {
            node,
            author,
            doc,
            ticket,
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
        self.insert_bytes(id.as_bytes(), task.as_vec()?).await
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
        let mut entries = self.doc.get_many(iroh::sync::store::GetFilter::All).await?;
        let mut hash_entries: HashMap<Vec<u8>, Entry> = HashMap::new();

        // only get most recent entry for the key
        // wish this had an easier api -> get_latest_for_each_key?
        while let Some(entry) = entries.next().await {
            let entry = entry?;
            let id = entry.id();
            if let Some(other_entry) = hash_entries.get(id.key()) {
                let other_timestamp = other_entry.record().timestamp();
                let this_timestamp = entry.record().timestamp();
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

    async fn insert_bytes(&self, key: impl AsRef<[u8]>, content: Vec<u8>) -> anyhow::Result<()> {
        self.doc
            .set_bytes(self.author, key.as_ref().to_vec(), content)
            .await?;
        Ok(())
    }

    async fn update_task(&mut self, key: impl AsRef<[u8]>, task: Task) -> anyhow::Result<()> {
        let content = task.as_vec()?;
        self.insert_bytes(key, content).await
    }

    async fn get_task(&self, id: String) -> anyhow::Result<Task> {
        // TODO - b5 - how are Getfilter's ordered in this context? latest timestamp? I hope so
        let entry = self
            .doc
            .get_many(iroh::sync::store::GetFilter::Prefix(id.as_bytes().to_vec()))
            .await?
            .next()
            .await
            .ok_or_else(|| anyhow::anyhow!("no task found"))??;

        // let entry = self.doc.get_latest_by_key(id.as_bytes().to_vec()).await?;
        self.task_from_entry(&entry).await
    }

    async fn task_from_entry(&self, entry: &Entry) -> anyhow::Result<Task> {
        let id = String::from_utf8(entry.key().to_owned())?;
        match self.node.blobs.read_to_bytes(entry.content_hash()).await {
            Ok(b) => Task::from_bytes(b),
            Err(_) => Ok(Task::missing_task(id)),
        }
    }
}
