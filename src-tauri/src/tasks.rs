use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{bail, Result};
use bytes::Bytes;
use iroh::sync::{Doc, DocStore};
use iroh_sync::sync::{RecordIdentifier, SignedEntry};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, Mutex};

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Task in a list of tasks
pub struct Task {
    pub id: uuid::Uuid,
    /// Description of the task
    /// Limited to 2000 characters
    pub description: String,
    /// Record creation timestamp. Counted as micros since the Unix epoch.
    pub created: u64,
    /// Whether or not the task has been completed. Done tasks will show up in the task list until
    /// they are archived.
    pub done: bool,
    /// Archive indicates whether we should display the task
    pub archived: bool,
}

const MAX_TASK_SIZE: usize = 2 * 1024;
const MAX_DESCRIPTION_LEN: usize = 2 * 1000;

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

    fn missing_task() -> Self {
        Self {
            description: String::from("Missing Content"),
            created: 0,
            done: false,
            archived: false,
        }
    }
}

/// List of tasks, including completed tasks that have not been archived
pub struct Tasks {
    inner: Arc<Mutex<InnerTasks>>,
    pub(super) handle: tokio::task::JoinHandle<()>,
}

struct InnerTasks {
    tasks: HashMap<uuid::Uuid, (RecordIdentifier, Task)>,
    doc: Doc,
}

/// TODO: make actual error
pub enum UpdateError {
    NoMoreUpdates,
    DeserializeTask,
    AddingTask,
}

impl Tasks {
    pub async fn new(
        doc: Doc,
        mut updates: mpsc::Receiver<(RecordIdentifier, SignedEntry)>,
    ) -> Result<(Self, oneshot::Receiver<UpdateError>)> {
        let entries = doc.replica().all();
        let mut tasks = HashMap::default();
        for (id, entry) in entries.into_iter() {
            match doc.get_content_bytes(&entry).await {
                None => tasks.insert((id, Task::missing_task())),
                Some(content) => {
                    let task = Task::from_bytes(content)?;
                    if !task.archived {
                        tasks.push((id, task))
                    }
                }
            }
        }
        tasks.sort_by_key(|(_, task)| task.created);
        let inner = Arc::new(Mutex::new(InnerTasks { doc, tasks }));
        let inner_clone = Arc::clone(&inner);
        let (sender, receiver) = oneshot::channel();
        let handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = tokio::signal::ctrl_c() => {
                        return;
                    }
                    res = updates.recv() => {
                        match res {
                            Some((id, entry)) => {
                                let mut inner = inner_clone.lock().await;
                                let doc = &inner.doc;
                                let content = doc.get_content_bytes(&entry).await;
                                let task = match content {
                                    Some(content) => {
                                        match Task::from_bytes(content) {
                                            Ok(task) => task,
                                            Err(_) => {
                                                    let _ = sender.send(UpdateError::DeserializeTask);
                                                    return;
                                            }
                                        }
                                    },
                                    None => Task::missing_task(),
                                };
                                match inner.insert_task(id, task) {
                                    Ok(_) => {},
                                    Err(_) => {
                                        let _ = sender.send(UpdateError::AddingTask);
                                        return;
                                    }
                                }
                            },
                            None => {
                                let _ = sender.send(UpdateError::NoMoreUpdates);
                                return;
                            }
                        }
                    }
                }
            }
        });
        Ok((Self { inner, handle }, receiver))
    }

    pub async fn save(&self, store: &DocStore) -> Result<()> {
        let inner = self.inner.lock().await;
        inner.save(store).await
    }

    pub async fn new_task(&mut self, description: String) -> Result<()> {
        if description.len() > MAX_DESCRIPTION_LEN {
            bail!("The task description must be under {MAX_DESCRIPTION_LEN} characters");
        }
        let id = uuid::Uuid::new_v4();
        let created = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .expect("time drift")
            .as_secs();
        let task = Task {
            description,
            created,
            done: false,
            archived: false,
        };
        self.insert_bytes(id.as_bytes(), task.as_bytes()?).await
    }

    async fn insert_bytes(&self, key: impl AsRef<[u8]>, content: Bytes) -> Result<()> {
        let inner = self.inner.lock().await;
        inner.doc.insert_bytes(key, content).await?;
        Ok(())
    }

    pub async fn update_task(&mut self, key: impl AsRef<[u8]>, task: Task) -> Result<()> {
        let content = task.as_bytes()?;
        self.insert_bytes(key, content).await
    }

    pub async fn toggle_done(&mut self, index: uuid::Uuid) -> Result<()> {
        let (id, mut task) = {
            let inner = self.inner.lock().await;
            inner.get_task(index)?
        };
        task.done = !task.done;
        self.update_task(id.key(), task).await
    }

    pub async fn delete(&mut self, index: uuid::Uuid) -> Result<()> {
        let (id, mut task) = {
            let inner = self.inner.lock().await;
            inner.get_task(index)?
        };
        task.archived = true;
        self.update_task(id.key(), task).await
    }

    pub async fn list(&self) -> Vec<(uuid::Uuid, Task)> {
        let inner = self.inner.lock().await;
        let tasks = inner.tasks.iter().map(|(_, t)| t.clone()).collect();
        Ok(tasks)
    }
}

impl InnerTasks {
    fn insert_task(&mut self, id: RecordIdentifier, task: Task) -> Result<()> {
        if let Some(index) = self.tasks.iter().position(|(tid, _)| id.key() == tid.key()) {
            if task.archived {
                self.tasks.remove(index);
            } else {
                let _ = std::mem::replace(&mut self.tasks[index], (id, task));
            }
        } else {
            if !task.archived {
                self.tasks.push((id, task));
            }
        }

        self.tasks.sort_by_key(|(_, task)| task.created);
        Ok(())
    }

    fn get_task(&self, index: uuid::Uuid) -> Result<(RecordIdentifier, Task)> {
        match self.tasks.get(index) {
            Some((id, task)) => Ok((id.to_owned(), task.clone())),
            None => bail!("No task exists at index {index}"),
        }
    }

    async fn save(&self, store: &DocStore) -> Result<()> {
        store.save(&self.doc).await
    }
}
