import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { LanProvider } from "./tools/lanContext";
import "./styles.css";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <LanProvider>
      <App />
    </LanProvider>
  </React.StrictMode>
);
