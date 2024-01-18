#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]
mod iroh_node;
mod todos;

use anyhow::Result;
use futures::StreamExt;
use iroh::client::LiveEvent;
use iroh::net::{defaults::default_derp_map, key::SecretKey};
use tauri::Manager;
use tokio::sync::{mpsc, oneshot};

use self::todos::{Todo, Todos};

struct AppState {}

fn main() {
    tauri::Builder::default()
        .manage(AppState {})
        .setup(|app| {
            let handle = app.handle();
            #[cfg(debug_assertions)] // only include this code on debug builds
            {
                let window = app.get_window("main").unwrap();
                window.open_devtools();
            }
            tauri::async_runtime::spawn(async move {
                println!("starting backend...");
                if let Err(err) = run(handle).await {
                    eprintln!("failed: {:?}", err);
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            new_list,
            get_ticket,
            get_todos,
            new_todo,
            toggle_done,
            update_todo,
            delete,
            set_ticket,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[derive(Debug)]
enum Cmd {
    /// Create a new todo
    Add { id: String, label: String },
    /// Change description of the todo
    Update { id: String, label: String },
    /// Toggle whether or not the todo is done
    ToggleDone { id: String },
    /// Remove a todo
    Delete { id: String },
    /// List all the todos
    Ls,
    /// Get the ticket for the current list
    GetTicket,
    /// Join a todo list
    SetTicket { ticket: String },
    /// Create a new todo list
    NewList,
}

#[derive(Debug)]
enum CmdAnswer {
    Ok,
    UnexpectedCmd,
    Ls(Vec<(String, Todo)>),
    Ticket(String),
}

#[tauri::command]
async fn get_todos(
    cmd_tx: tauri::State<'_, mpsc::Sender<(Cmd, oneshot::Sender<CmdAnswer>)>>,
) -> Result<Vec<Todo>, String> {
    println!("get_todos called from app");
    let (tx, rx) = oneshot::channel();
    cmd_tx.send((Cmd::Ls, tx)).await.unwrap();
    if let CmdAnswer::Ls(todos) = rx.await.unwrap() {
        let todos = todos.into_iter().map(|(_id, t)| t).collect();
        return Ok(todos);
    }
    unreachable!("invalid answer");
}

#[tauri::command]
async fn new_list(
    cmd_tx: tauri::State<'_, mpsc::Sender<(Cmd, oneshot::Sender<CmdAnswer>)>>,
) -> Result<(), String> {
    println!("new_list called from app");
    let (tx, rx) = oneshot::channel();
    cmd_tx.send((Cmd::NewList, tx)).await.unwrap();
    rx.await.unwrap();
    Ok(())
}

#[tauri::command]
async fn new_todo(
    todo: Todo,
    cmd_tx: tauri::State<'_, mpsc::Sender<(Cmd, oneshot::Sender<CmdAnswer>)>>,
) -> Result<(), String> {
    println!("new_todo called from app");
    let label = todo.label.clone();
    let id = todo.id.clone();
    let (tx, rx) = oneshot::channel();
    cmd_tx.send((Cmd::Add { id, label }, tx)).await.unwrap();
    rx.await.unwrap();
    Ok(())
}

#[tauri::command]
async fn update_todo(
    todo: Todo,
    cmd_tx: tauri::State<'_, mpsc::Sender<(Cmd, oneshot::Sender<CmdAnswer>)>>,
) -> Result<(), String> {
    println!("update_todo called from app");
    let id = todo.id;
    let label = todo.label;
    let (tx, rx) = oneshot::channel();
    cmd_tx.send((Cmd::Update { id, label }, tx)).await.unwrap();
    rx.await.unwrap();
    Ok(())
}

#[tauri::command]
async fn toggle_done(
    // state: tauri::State<'_, AppState>,
    id: String,
    cmd_tx: tauri::State<'_, mpsc::Sender<(Cmd, oneshot::Sender<CmdAnswer>)>>,
) -> Result<bool, String> {
    println!("toggle_done called from app");
    let (tx, rx) = oneshot::channel();
    cmd_tx.send((Cmd::ToggleDone { id }, tx)).await.unwrap();
    rx.await.unwrap();
    Ok(true)
}

#[tauri::command]
async fn delete(
    // state: tauri::State<'_, AppState>,
    id: String,
    cmd_tx: tauri::State<'_, mpsc::Sender<(Cmd, oneshot::Sender<CmdAnswer>)>>,
) -> Result<bool, String> {
    println!("delete called from app");
    let (tx, rx) = oneshot::channel();
    cmd_tx.send((Cmd::Delete { id }, tx)).await.unwrap();
    rx.await.unwrap();
    Ok(true)
}

#[tauri::command]
async fn set_ticket(
    ticket: String,
    cmd_tx: tauri::State<'_, mpsc::Sender<(Cmd, oneshot::Sender<CmdAnswer>)>>,
) -> Result<(), String> {
    println!("set_ticket called from app");
    let (tx, rx) = oneshot::channel();
    cmd_tx.send((Cmd::SetTicket { ticket }, tx)).await.unwrap();
    rx.await.unwrap();
    Ok(())
}

#[tauri::command]
async fn get_ticket(
    cmd_tx: tauri::State<'_, mpsc::Sender<(Cmd, oneshot::Sender<CmdAnswer>)>>,
) -> Result<String, String> {
    println!("get_ticket called from app");
    let (tx, rx) = oneshot::channel();
    cmd_tx.send((Cmd::GetTicket, tx)).await.unwrap();
    if let CmdAnswer::Ticket(ticket) = rx.await.unwrap() {
        return Ok(ticket);
    }
    unreachable!("invalid answer");
}

async fn run<R: tauri::Runtime>(handle: tauri::AppHandle<R>) -> Result<()> {
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<(Cmd, oneshot::Sender<CmdAnswer>)>(8);
    handle.manage(cmd_tx);

    let keypair = SecretKey::generate();
    let derp_map = default_derp_map();

    let iroh = iroh_node::Iroh::new(keypair, Some(derp_map), None).await?;

    let mut todos = None;
    println!("waiting for todo create command...");
    while let Some((cmd, answer_tx)) = cmd_rx.recv().await {
        let iroh_client = iroh.client();
        let todos_updater = handle.clone();
        todos = handle_start_command(todos_updater, cmd, answer_tx, iroh_client).await?;
        if todos.is_some() {
            println!("todos created!");
            break;
        }
    }

    let mut todos = todos.expect("checked above");
    println!("todos list created!");

    let mut events = todos.doc_subscribe().await?;
    let events_handle = tokio::spawn(async move {
        while let Some(Ok(event)) = events.next().await {
            match event {
                LiveEvent::InsertRemote { .. } => {
                    // TODO: need an `on_download` callback, this sleep
                    // allows for downloading the content before trying to get it
                    // from the store
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
                _ => {}
            }
            handle
                .emit_all("update-all", ())
                .expect("unable to send update event");
        }
    });

    println!("waiting for cmds...");
    while let Some((cmd, answer_tx)) = cmd_rx.recv().await {
        let res = handle_command(cmd, &mut todos, answer_tx).await;
        if let Err(err) = res {
            println!("> error: {err}");
        }
    }

    println!("shutting down todos list...");
    events_handle.abort();
    iroh.shutdown();
    Ok(())
}

async fn handle_start_command<R: tauri::Runtime>(
    _handle: tauri::AppHandle<R>,
    cmd: Cmd,
    answer_tx: oneshot::Sender<CmdAnswer>,
    iroh: iroh::client::mem::Iroh,
) -> Result<Option<Todos>> {
    match cmd {
        Cmd::NewList => {
            println!("new list");
            let todos = Todos::new(None, iroh).await?;
            answer_tx.send(CmdAnswer::Ok).unwrap();
            Ok(Some(todos))
        }
        Cmd::SetTicket { ticket } => {
            println!("set ticket: {ticket}");
            let todos = Todos::new(Some(ticket), iroh).await?;
            answer_tx.send(CmdAnswer::Ok).unwrap();
            Ok(Some(todos))
        }
        _ => {
            answer_tx.send(CmdAnswer::UnexpectedCmd).unwrap();
            Ok(None)
        }
    }
}

async fn handle_command(
    cmd: Cmd,
    todo: &mut Todos,
    answer_tx: oneshot::Sender<CmdAnswer>,
) -> Result<()> {
    match cmd {
        Cmd::Add { id, label } => {
            println!("adding todo: {label}");
            todo.add(id, label).await?;
            answer_tx.send(CmdAnswer::Ok).unwrap();
        }
        Cmd::ToggleDone { id } => {
            todo.toggle_done(id).await?;
            answer_tx.send(CmdAnswer::Ok).unwrap();
        }
        Cmd::Delete { id } => {
            todo.delete(id).await?;
            answer_tx.send(CmdAnswer::Ok).unwrap();
        }
        Cmd::Ls => {
            let todos = todo.get_todos().await?;
            let todos = todos.into_iter().map(|t| (t.id.clone(), t)).collect();
            answer_tx.send(CmdAnswer::Ls(todos)).unwrap();
        }
        Cmd::GetTicket => {
            println!("getting ticket");
            answer_tx.send(CmdAnswer::Ticket(todo.ticket())).unwrap();
        }
        Cmd::Update { id, label } => {
            todo.update(id, label).await?;
            answer_tx.send(CmdAnswer::Ok).unwrap();
        }
        cmd => {
            answer_tx.send(CmdAnswer::UnexpectedCmd).unwrap();
            anyhow::bail!("unexpected command {cmd:?}");
        }
    }

    Ok(())
}
