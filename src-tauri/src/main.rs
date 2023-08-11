#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]
mod iroh_bytes_handler;
mod tasks;
mod todo;

use anyhow::Result;
use std::sync::Mutex;
use tauri::Manager;
use tokio::sync::{mpsc, oneshot};

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
    Update {id: String, label: String},
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
    Ls(Vec<(String, Task)>),
    Ticket(String)
}

#[tauri::command]
async fn get_todos(
    cmd_tx: tauri::State<'_, mpsc::Sender<(Cmd, oneshot::Sender<CmdAnswer>)>>,
) -> Result<Vec<Todo>, String> {
    println!("get_todos called from app");
    let (tx, rx) = oneshot::channel();
    cmd_tx.send((Cmd::Ls, tx)).await.unwrap();
    if let CmdAnswer::Ls(tasks) = rx.await.unwrap() {
        let todos = tasks
            .into_iter()
            .map(|(id, t)| Todo {
                id,
                label: t.label,
                done: t.done,
                is_delete: t.is_delete,
            })
            .collect();
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
    cmd_tx
        .send((Cmd::NewList, tx))
        .await
        .unwrap();
    rx.await.unwrap();
    Ok(())
}

#[tauri::command]
async fn new_todo(
    state: tauri::State<'_, AppState>,
    todo: Todo,
    cmd_tx: tauri::State<'_, mpsc::Sender<(Cmd, oneshot::Sender<CmdAnswer>)>>,
) -> Result<bool, String> {
    println!("new_todo called from app");
    let label = todo.label.clone();
    let id = todo.id.clone();
    let result = {
        let app = state.app.lock().unwrap();
        app.new_todo(todo)
    };
    let (tx, rx) = oneshot::channel();
    cmd_tx
        .send((Cmd::Add { id, label }, tx))
        .await
        .unwrap();
    rx.await.unwrap();
    Ok(result)
}

#[tauri::command]
async fn update_todo(
    state: tauri::State<'_, AppState>,
    todo: Todo,
    cmd_tx: tauri::State<'_, mpsc::Sender<(Cmd, oneshot::Sender<CmdAnswer>)>>,
) -> Result<(), String> {
    println!("update_todo called from app");
    let id = todo.id;
    let label = todo.label;
    let (tx, rx) = oneshot::channel();
    cmd_tx
        .send((Cmd::Update {id, label},  tx))
        .await
        .unwrap();
    rx.await.unwrap();
    Ok(())
}

#[tauri::command]
async fn toggle_done(
    state: tauri::State<'_, AppState>,
    id: String,
    cmd_tx: tauri::State<'_, mpsc::Sender<(Cmd, oneshot::Sender<CmdAnswer>)>>,
) -> Result<bool, String> {
    println!("toggle_done called from app");
    let (tx, rx) = oneshot::channel();
    cmd_tx
        .send((Cmd::ToggleDone {id},  tx))
        .await
        .unwrap();
    rx.await.unwrap();
    Ok(true)
}

#[tauri::command]
async fn delete(
    state: tauri::State<'_, AppState>,
    id: String,
    cmd_tx: tauri::State<'_, mpsc::Sender<(Cmd, oneshot::Sender<CmdAnswer>)>>,
) -> Result<bool, String> {
    println!("delete called from app");
    let (tx, rx) = oneshot::channel();
    cmd_tx
        .send((Cmd::Delete {id},  tx))
        .await
        .unwrap();
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
    cmd_tx
        .send((Cmd::SetTicket { ticket }, tx))
        .await
        .unwrap();
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

    let mut tasks = None;
    println!("waiting for task create command...");
    while let Some((cmd, answer_tx)) = cmd_rx.recv().await {
        let tasks_updater = handle.clone();
        tasks = handle_start_command(tasks_updater, cmd, answer_tx).await?;
        if tasks.is_some() { 
            println!("tasks created!");
            break; 
        }
    }

    let mut tasks = tasks.expect("checked above");
    println!("tasks list created!");

    println!("waiting for cmds...");
    while let Some((cmd, answer_tx)) = cmd_rx.recv().await {
        let res = handle_command(cmd, &mut tasks, answer_tx).await;
        if let Err(err) = res {
            println!("> error: {err}");
        }
    }

    println!("shutting down tasks list...");
    tasks.shutdown().await?;

    handle
        .emit_all("update-all", ())
        .expect("unable to send event");

    Ok(())
}

    async fn handle_start_command<R: tauri::Runtime>(handle: tauri::AppHandle<R>, cmd: Cmd, answer_tx: oneshot::Sender<CmdAnswer>) -> Result<Option<Tasks>> {
    let on_insert = Box::new(move |_origin, _entry| {
        handle.emit_all("update-all", ())
            .expect("unable to send update event");
    });
    match cmd {
        Cmd::NewList => {
            println!("new list");
            let tasks = Tasks::new(None, Some(on_insert)).await?;
            answer_tx.send(CmdAnswer::Ok).unwrap();
            Ok(Some(tasks))
        }
        Cmd::SetTicket {ticket } => {
            println!("set ticket: {ticket}");
            let tasks = Tasks::new(Some(ticket), Some(on_insert)).await?;
            answer_tx.send(CmdAnswer::Ok).unwrap();
            Ok(Some(tasks))
        }
        _ => {
            answer_tx.send(CmdAnswer::UnexpectedCmd).unwrap();
            Ok(None)
        }
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
    answer_tx: oneshot::Sender<CmdAnswer>,
) -> Result<()> {
    match cmd {
        Cmd::Add { id, label } => {
            println!("adding todo: {label}");
            task.add(id, label).await?;
            answer_tx.send(CmdAnswer::Ok).unwrap();
        }
        Cmd::ToggleDone { id } => {
            task.toggle_done(id).await?;
            answer_tx.send(CmdAnswer::Ok).unwrap();
        }
        Cmd::Delete { id } => {
            task.delete(id).await?;
            answer_tx.send(CmdAnswer::Ok).unwrap();
        }
        Cmd::Ls => {
            let tasks = task.get_tasks().await?;
            let tasks = tasks.into_iter().map(|t| (t.id.clone(), t)).collect();
            answer_tx.send(CmdAnswer::Ls(tasks)).unwrap();
        }
        Cmd::GetTicket => {
            println!("getting ticket");
            answer_tx.send(CmdAnswer::Ticket(task.ticket())).unwrap();
        }
        Cmd::Update { id, label } => {
            task.update(id, label).await?;
            answer_tx.send(CmdAnswer::Ok).unwrap();
        }
        cmd => {
            answer_tx.send(CmdAnswer::UnexpectedCmd).unwrap();
            anyhow::bail!("unexpected command {cmd:?}");
        }
    }

    Ok(())
}
