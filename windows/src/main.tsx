import { render } from "preact";
import { App } from "./App";
import { store } from "./store";
import "@fontsource-variable/bricolage-grotesque";
import "@fontsource/instrument-sans/400.css";
import "@fontsource/instrument-sans/500.css";
import "@fontsource/instrument-sans/600.css";
import "@fontsource/martian-mono/400.css";
import "@fontsource/martian-mono/600.css";
import "./styles.css";

// Same lifecycle as ACSApp.swift: restore the last workspace, then refresh
// the snapshot on the same 4-second cadence.
void store.restore();
setInterval(() => void store.refresh(), 4000);

render(<App />, document.getElementById("app")!);
