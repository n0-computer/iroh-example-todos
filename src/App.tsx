import { invoke } from '@tauri-apps/api'
import { listen } from '@tauri-apps/api/event'
import { useAtom } from 'jotai'
import { useEffect, useState } from 'react'
import TodoList from './component/TodoList'
import OpenList from './component/OpenList'
import { allTodosAtom, filterAtom } from './store/todos'
import { Todo } from './types/todo'

function App() {
  const [, setAllTodos] = useAtom(allTodosAtom)
  const [todos] = useAtom(filterAtom)
  const [name, setName] = useState("");
  const [showOpenList, setShowOpenList] = useState(true);
  const [todoLists, setTodoLists] = useState<string[]>([]);

  useEffect(() => {
    listen('update-all', (event) => {
      console.log("updating", event)
      getTodos() 
    })
    getTodoLists();
  }, [])

  function createList(name: string) {
    console.log("create list");
    if (name.length == 0) {
      return;
    }
    invoke('new_list', { name }).then(() => {
      console.log("in new_list and then");
       getTodos(); 
    })
  }

  function getTodoLists() {
    console.log("get todo list");
    invoke<string[]>('get_todo_lists').then((res) => {
      console.log(res);
      setTodoLists(() => [...res]);
    })
  }

  function openList(name: string) {
    console.log("create list");
    if (name.length == 0) {
      return;
    }
    invoke('open_list', { name }).then(() => {
       getTodos(); 
    })
  }  

  function joinList(ticket:string) {
    console.log("join list");
    if (ticket.length == 0) {
      return;
    }
    setTicket(ticket);
    getTodos();
  }

  function setTicket(ticket: string) {
    // this is the effect for the modal
    // otherwise just get-todos
    invoke('set_ticket', {ticket}).then(() => {
      getTodos()
    })
  }

  function getTodos() {
    invoke<Todo[]>('get_todos').then((res) => {
      setAllTodos(res)
    }).then(()=> {
      console.log("calling get list name after get todos");
      invoke<string>('get_list_name').then((res) => {
        console.log("name is", res);
        setName(res)
      })
    }).then(() => {
      console.log("setting show open list");
      setShowOpenList(false)  
    })
  }

  return (
    <div className="todoapp">
      {showOpenList && <OpenList createList={createList} joinList={joinList} openList={openList} todoLists={todoLists}/>}
      {!showOpenList && <TodoList todos={todos} name={name}/>}
    </div>
  )
}

export default App
