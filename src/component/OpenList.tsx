import { useState } from 'react'
import Select, { } from 'react-select'

const OpenList: React.FC<{ createList: (name: string) => void, joinList: (ticket: string) => void, openList: (name: string) => void, todoLists: string[]}> = ({ createList, joinList, openList, todoLists}) => {
    const [chosenList, setChosenList] = useState({ value: '', label: ''});
    const [name, setName] = useState('');
    const [ticket, setTicket] = useState('');

    const onOpenList = () => {
      if (chosenList.value.length > 0) {
        openList(chosenList.value);
      }
    }

  return (
    <>
      <header className="header">
        <h1>todos</h1>
      </header>
      <footer className="footer" style={{ height:"auto"}}>
          <p className="instructions">Open a new todo list or input a ticket to join an active one:</p>
          <div className="listOptions">
            <input 
                className="textInput"
                value={name}
                onChange={(e) => { setName(e.target.value) }}
                type="text" 
                placeholder='new todo list name'
            />
            <a className="link" onClick={() => createList(name)}>Create New List ⇨</a>
          </div>
          { todoLists.length > 0 && 
          <div className="listOptions">
            <Select
              styles={{
                container: (baseStyles) => (
                  {
                    ...baseStyles,
                    flexGrow: 1,
                    marginRight: 10,
                    fontSize: 14,
                    textAlign: 'left',
                  }),
                  control: (baseStyles) => ({
                    ...baseStyles,
                    background: "#f5f5f5",    
                  }),
                  input: (baseStyles) => ({
                    ...baseStyles,
                    paddingLeft: 10,    
                  }),

              }}
              defaultValue={chosenList}
              onChange={(val) => {
                let obj = val === null ?
                  { value: '', label: ''} :
                  { value: val.value, label: val.label};
                setChosenList(obj)
              }}
              options={todoLists.map((list) => ({ value: list, label: list }))}
            />
            <a className="link" onClick={onOpenList}>Open List ⇨</a>
          </div>}
          <div className="listOptions">
            <input 
                className="textInput"
                value={ticket}
                onChange={(e) => { setTicket(e.target.value) }}
                type="text" 
                placeholder='input a ticket to join a list'
            />
            <a className="link" onClick={() => joinList(ticket)}>Join Using Ticket ⇨</a>
          </div>
      </footer>
    </>
  )
}
export default OpenList
