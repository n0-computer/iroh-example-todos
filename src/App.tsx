import { invoke } from '@tauri-apps/api'
import { listen } from '@tauri-apps/api/event'
import { useAtom } from 'jotai'
import { useEffect } from 'react'
import TodoList from './component/TodoList'
import { allTodosAtom, filterAtom } from './store/todos'
import { Todo } from './types/todo'

function App() {
  const [, setAllTodos] = useAtom(allTodosAtom)
  const [todos] = useAtom(filterAtom)

  useEffect(() => {
    invoke<Todo[]>('get_todos').then((res) => {
      setAllTodos(res)
    })

    listen('update-all', (event) => {
      console.log("updating", event)
      invoke<Todo[]>('get_todos').then((res) => {
        setAllTodos(res)
      })
    })
  }, [])

  return (
    <div className="todoapp">
      <TodoList todos={todos} />
    </div>
  )
}

export default App
