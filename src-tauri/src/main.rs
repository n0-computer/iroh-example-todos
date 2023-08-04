#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]
mod iroh_bytes_handler;
mod tasks;
mod todo;

use anyhow::{bail, Result};
use iroh::net::{
    defaults::default_derp_map, magic_endpoint::get_alpn, tls::Keypair, MagicEndpoint,
};
use iroh::sync::{BlobStore, Doc, DocStore, DownloadMode, LiveSync, PeerSource, SYNC_ALPN};
use iroh_gossip::{
    net::{GossipHandle, GOSSIP_ALPN},
    proto::TopicId,
};
use iroh_sync::sync::{Author, Namespace};
use once_cell::sync::OnceCell;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use tauri::Manager;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use self::iroh_bytes_handler::IrohBytesHandlers;
use self::tasks::{Task, Tasks};
use self::todo::{Todo, TodoApp};

struct AppState {
    app: Mutex<TodoApp>,
}

fn main() {
    let app = TodoApp::new().unwrap();
    tauri::Builder::default()
        .manage(AppState {
            app: Mutex::from(app),
        })
        .setup(|app| {
            let handle = app.handle();
            tauri::async_runtime::spawn(async move {
                if let Err(err) = run(handle).await {
                    eprintln!("failed: {:?}", err);
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_todos,
            new_todo,
            toggle_done,
            update_todo
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[derive(Debug)]
enum Cmd {
    Add { description: String },
    ToggleDone { index: usize },
    Delete { index: usize },
    Ls,
}

#[derive(Debug)]
enum CmdAnswer {
    Ok,
    Ls(Vec<(Uuid, Task)>),
}

#[tauri::command]
async fn get_todos(
    cmd_tx: tauri::State<'_, mpsc::Sender<(Cmd, oneshot::Sender<CmdAnswer>)>>,
) -> Result<Vec<Todo>, String> {
    let (tx, rx) = oneshot::channel();
    cmd_tx.send((Cmd::Ls, tx)).await.unwrap();
    if let CmdAnswer::Ls(tasks) = rx.await.unwrap() {
        // TODO: update frontend
        let todos = tasks
            .into_iter()
            .map(|(id, t)| Todo {
                id: id.to_string(),
                label: t.description,
                done: t.done,
                is_delete: t.archived,
            })
            .collect();
        return Ok(todos);
    }
    unreachable!("invalid answer");
}

#[tauri::command]
async fn new_todo(
    state: tauri::State<'_, AppState>,
    todo: Todo,
    cmd_tx: tauri::State<'_, mpsc::Sender<(Cmd, oneshot::Sender<CmdAnswer>)>>,
) -> Result<bool, String> {
    let description = todo.label.clone();
    let result = {
        let app = state.app.lock().unwrap();
        app.new_todo(todo)
    };
    let (tx, rx) = oneshot::channel();
    cmd_tx.send((Cmd::Add { description }, tx)).await.unwrap();
    rx.await.unwrap();
    Ok(result)
}

#[tauri::command]
fn update_todo(state: tauri::State<AppState>, todo: Todo) -> bool {
    let app = state.app.lock().unwrap();
    let result = app.update_todo(todo);
    result
}

#[tauri::command]
fn toggle_done(state: tauri::State<AppState>, id: String) -> bool {
    let app = state.app.lock().unwrap();
    let Todo {
        id,
        label,
        done,
        is_delete,
    } = app.get_todo(id).unwrap();
    let result = app.update_todo(Todo {
        id,
        label,
        done: !done,
        is_delete,
    });
    result
}

async fn run<R: tauri::Runtime>(handle: tauri::AppHandle<R>) -> Result<()> {
    let keypair = Keypair::generate();
    let derp_map = Some(default_derp_map());
    let bind_port = 0;

    let (endpoint, gossip, initial_endpoints) = {
        let gossip_cell: OnceCell<GossipHandle> = OnceCell::new();
        let (initial_endpoints_tx, mut initial_endpoints_rx) = mpsc::channel(1);
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
                    if let Some(gossip) = gossip_cell.get() {
                        gossip.update_endpoints(endpoints).ok();
                    }
                    initial_endpoints_tx.try_send(endpoints.to_vec()).ok();
                })
            })
            .bind(bind_port)
            .await?;

        let gossip = GossipHandle::from_endpoint(endpoint.clone(), Default::default());
        gossip_cell.set(gossip.clone()).unwrap();
        let initial_endpoints = initial_endpoints_rx.recv().await.unwrap();
        gossip.update_endpoints(&initial_endpoints)?;
        (endpoint, gossip, initial_endpoints)
    };

    println!("> our peer id: {}", endpoint.peer_id());

    // TODO: allow opening an existing one
    let doc_name = "tasks-01";
    let topic: TopicId = blake3::hash(doc_name.as_bytes()).into();
    let mut peers = Vec::new();

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
    println!("> ticket to join us: {our_ticket}");

    // TODO: use tauri folder?
    let storage_path = {
        let name = format!("iroh-todo-{}", endpoint.peer_id());
        let dir = std::env::temp_dir().join(name);
        if !dir.exists() {
            std::fs::create_dir(&dir).expect("failed to create temp dir");
        }
        dir
    };
    println!("> storage directory: {storage_path:?}");

    // create a runtime that can spawn tasks on a local-thread executors (to support !Send futures)
    let rt = iroh::bytes::util::runtime::Handle::from_currrent(num_cpus::get())?;

    // create a blob store (with a iroh-bytes database inside)
    let blobs = BlobStore::new(rt.clone(), storage_path.join("blobs"), endpoint.clone()).await?;

    // create a doc store for the iroh-sync docs
    let author = Author::from(keypair.secret().clone());
    let docs = DocStore::new(blobs.clone(), author, storage_path.join("docs"));

    // create the live syncer
    let live_sync = LiveSync::spawn(endpoint.clone(), gossip.clone());

    // construct the state that is passed to the endpoint loop and from there cloned
    // into to the connection handler task for incoming connections.
    let state = Arc::new(State {
        gossip: gossip.clone(),
        docs: docs.clone(),
        bytes: IrohBytesHandlers::new(rt.clone(), blobs.db().clone()),
    });

    // spawn our endpoint loop that forwards incoming connections
    tokio::spawn(endpoint_loop(endpoint.clone(), state));

    // open our document and add to the live syncer
    let namespace = Namespace::from_bytes(topic.as_bytes());
    println!("> opening doc {}", fmt_hash(namespace.id().as_bytes()));
    let doc = docs.create_or_open(namespace, DownloadMode::Always).await?;
    live_sync.add(doc.replica().clone(), peers.clone()).await?;

    // TODO: use this to send commands from the app
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<(Cmd, oneshot::Sender<CmdAnswer>)>(8);
    handle.manage(cmd_tx);

    let (send, recv) = mpsc::channel(32);
    doc.on_insert(Box::new(move |_origin, entry| {
        send.try_send((entry.entry().id().to_owned(), entry))
            .expect("receiver dropped");
    }));
    let (mut tasks, mut update_errors) = Tasks::new(doc, recv).await?;

    while let Some((cmd, answer_tx)) = cmd_rx.recv().await {
        // handle the command, but select against Ctrl-C signal so that commands can be aborted
        tokio::select! {
            biased;
            _ = &mut update_errors => {
                // TODO: when err is actual error print it
                println!("> error updating tasks");
                break;
            }
            res = handle_command(cmd, &mut tasks, &our_ticket, answer_tx) => if let Err(err) = res {
                println!("> error: {err}");
            },
        };
    }

    if let Err(err) = live_sync.cancel().await {
        println!("> syncer closed with error: {err:?}");
    }
    println!("> persisting tasks {storage_path:?}");
    blobs.save().await?;
    tasks.save(&docs).await?;

    tasks.handle.abort();

    handle
        .emit_all("update-all", ())
        .expect("unable to send event");

    //     new_todo(
    //         handle.state(),
    //         Todo {
    //             id: Uuid::new_v4().to_string(),
    //             label: format!("fix backend: {i}"),
    //             done: false,
    //             is_delete: false,
    //         },
    //     );
    // }

    Ok(())
}

#[derive(Debug)]
struct State {
    gossip: GossipHandle,
    docs: DocStore,
    bytes: IrohBytesHandlers,
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

fn fmt_hash(hash: impl AsRef<[u8]>) -> String {
    let mut text = data_encoding::BASE32_NOPAD.encode(hash.as_ref());
    text.make_ascii_lowercase();
    format!("{}…{}", &text[..5], &text[(text.len() - 2)..])
}

async fn handle_command(
    cmd: Cmd,
    task: &mut Tasks,
    ticket: &Ticket,
    answer_tx: oneshot::Sender<CmdAnswer>,
) -> Result<()> {
    match cmd {
        Cmd::Add { description } => {
            println!("adding todo: {description}");
            task.new_task(description).await?;
            answer_tx.send(CmdAnswer::Ok).unwrap();
        }
        Cmd::ToggleDone { index } => {
            task.toggle_done(index).await?;
            answer_tx.send(CmdAnswer::Ok).unwrap();
        }
        Cmd::Delete { index } => {
            task.delete(index).await?;
            answer_tx.send(CmdAnswer::Ok).unwrap();
        }
        Cmd::Ls => {
            let tasks = task.list().await?;
            answer_tx.send(CmdAnswer::Ls(tasks)).unwrap();
        }
    }

    Ok(())
}
