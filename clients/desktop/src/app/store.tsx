// The app store: one useReducer behind two contexts (state and dispatch), so
// a component that only dispatches does not re-render on every change.

import { createContext, useContext, useEffect, useReducer, type Dispatch, type ReactNode } from "react";
import { reducer, type Action } from "@/app/reducer";
import { initialState, type AppState } from "@/app/state";
import { loadPrefs, savePrefs } from "@/app/prefs";

const StateContext = createContext<AppState | null>(null);
const DispatchContext = createContext<Dispatch<Action> | null>(null);

export function StoreProvider({ children, initial }: { children: ReactNode; initial?: AppState }) {
  const [state, dispatch] = useReducer(reducer, initial, (i) => i ?? initialState(loadPrefs()));
  useEffect(() => savePrefs(state.prefs), [state.prefs]);
  return (
    <DispatchContext.Provider value={dispatch}>
      <StateContext.Provider value={state}>{children}</StateContext.Provider>
    </DispatchContext.Provider>
  );
}

export function useAppState(): AppState {
  const s = useContext(StateContext);
  if (!s) throw new Error("useAppState outside StoreProvider");
  return s;
}

export function useDispatch(): Dispatch<Action> {
  const d = useContext(DispatchContext);
  if (!d) throw new Error("useDispatch outside StoreProvider");
  return d;
}
