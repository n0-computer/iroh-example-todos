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
    Add { id: String, description: String },
    ToggleDone { id: String },
    Delete { id: String },
    Ls,
}

#[derive(Debug)]
enum CmdAnswer {
    Ok,
    Ls(Vec<(String, Task)>),
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
async fn new_todo(
    state: tauri::State<'_, AppState>,
    todo: Todo,
    cmd_tx: tauri::State<'_, mpsc::Sender<(Cmd, oneshot::Sender<CmdAnswer>)>>,
) -> Result<bool, String> {
    let description = todo.label.clone();
    let id = todo.id.clone();
    let result = {
        let app = state.app.lock().unwrap();
        app.new_todo(todo)
    };
    let (tx, rx) = oneshot::channel();
    cmd_tx
        .send((Cmd::Add { id, description }, tx))
        .await
        .unwrap();
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
    let mut tasks = Tasks::new(None).await?;
    // TODO: use this to send commands from the app
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<(Cmd, oneshot::Sender<CmdAnswer>)>(8);
    handle.manage(cmd_tx);

    while let Some((cmd, answer_tx)) = cmd_rx.recv().await {
        let res = handle_command(cmd, &mut tasks, answer_tx).await;
        if let Err(err) = res {
            println!("> error: {err}");
        }
    }

    tasks.shutdown().await?;

    handle
        .emit_all("update-all", ())
        .expect("unable to send event");

    Ok(())
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
        Cmd::Add { id, description } => {
            println!("adding todo: {description}");
            task.add(id, description).await?;
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
    }

    Ok(())
}
