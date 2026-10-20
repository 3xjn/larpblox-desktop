import React from "react";
import { createRoot } from "react-dom/client";
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  Chrome,
  Gamepad2,
  Check,
  Copy,
  ArrowUpRight,
  LoaderCircle,
  Settings,
  RotateCw,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { Separator } from "@/components/ui/separator";
import { Switch } from "@/components/ui/switch";
import {
  Popover,
  PopoverTrigger,
  PopoverPopup,
  PopoverTitle,
} from "@/components/ui/popover";
import logo from "./assets/larpblox-logo.png";
import "./styles.css";

type Status = {
  ready: boolean;
  starting: boolean;
  browsers: number;
  games: number;
  startup: boolean;
  error: string | null;
};
function App() {
  const [status, setStatus] = useState<Status>({
    ready: false,
    starting: true,
    browsers: 0,
    games: 0,
    startup: false,
    error: null,
  });
  const [error, setError] = useState("");
  const [copied, setCopied] = useState(false);
  const [saving, setSaving] = useState(false);
  const [retrying, setRetrying] = useState(false);
  useEffect(() => {
    let active = true;
    const refresh = () =>
      invoke<Status>("connection_status")
        .then((value) => {
          if (active) setStatus(value);
        })
        .catch(() => {
          if (active)
            setError("Couldn’t read the connection. Reopen Larpblox to retry.");
        });
    refresh();
    const timer = setInterval(refresh, 1000);
    return () => {
      active = false;
      clearInterval(timer);
    };
  }, []);
  async function action(command: string) {
    try {
      await invoke(command);
      setError("");
    } catch {
      setError("That didn’t work. Please try again.");
    }
  }
  async function retryConnection() {
    setRetrying(true);
    setError("");
    try {
      await invoke("retry_connection");
    } catch (cause) {
      setError(String(cause));
    } finally {
      try {
        setStatus(await invoke<Status>("connection_status"));
      } catch {
        setError("Couldn’t read the connection. Please retry.");
      }
      setRetrying(false);
    }
  }
  async function copy() {
    try {
      await invoke("copy_launcher");
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch {
      setError("Couldn’t copy the game script.");
    }
  }
  async function startup(enabled: boolean) {
    setSaving(true);
    try {
      await invoke("set_startup", { enabled });
      setStatus((s) => ({ ...s, startup: enabled }));
      setError("");
    } catch {
      setError("Couldn’t update your startup setting.");
    } finally {
      setSaving(false);
    }
  }
  return (
    <main className="mx-auto flex h-screen max-w-lg flex-col px-7 pb-6 pt-6">
      <header className="mb-7 flex items-center justify-between">
        <img src={logo} alt="Larpblox" className="w-48" />
        <Popover>
          <PopoverTrigger
            render={
              <Button
                variant="ghost"
                size="icon"
                aria-label="Settings"
                title="Settings"
              />
            }
          >
            <Settings className="size-4 text-muted-foreground" />
          </PopoverTrigger>
          <PopoverPopup align="end" sideOffset={10} className="w-72">
            <PopoverTitle className="mb-4 text-sm">Settings</PopoverTitle>
            <label className="flex items-center justify-between gap-4">
              <div>
                <div className="text-sm">Start with Windows</div>
                <div className="mt-1 text-xs text-muted-foreground">
                  Open quietly in the system tray.
                </div>
              </div>
              <Switch
                checked={status.startup}
                disabled={saving}
                onCheckedChange={startup}
                aria-label="Start with Windows"
              />
            </label>
          </PopoverPopup>
        </Popover>
      </header>
      <section aria-labelledby="connections">
        <div className="mb-3 flex items-center justify-between">
          <h1 id="connections" className="text-sm font-medium">
            Connections
          </h1>
        </div>
        <div className="overflow-hidden rounded-xl border bg-card">
          <div className="flex min-h-24 items-center gap-4 px-5 py-5">
            <Chrome
              className={`size-5 shrink-0 transition-colors ${status.browsers ? "text-success-foreground" : "text-muted-foreground"}`}
            />
            <div className="min-w-0 flex-1">
              <h2 className="text-sm font-medium">Browser</h2>
              <p role="status" className="mt-1 text-xs text-muted-foreground">
                {status.browsers ? "Connected" : "Open Roblox to connect."}
              </p>
            </div>
            <Button
              variant="ghost"
              size="sm"
              onClick={() => action("open_roblox")}
            >
              Open <ArrowUpRight />
            </Button>
          </div>
          <Separator />
          <div className="flex min-h-24 items-center gap-4 px-5 py-5">
            <Gamepad2
              className={`size-5 shrink-0 transition-colors ${status.games ? "text-success-foreground" : "text-muted-foreground"}`}
            />
            <div className="min-w-0 flex-1">
              <h2 className="text-sm font-medium">Game</h2>
              <p role="status" className="mt-1 text-xs text-muted-foreground">
                {status.games ? "Connected" : "Run Larpblox to connect."}
              </p>
            </div>
            <Button
              variant="ghost"
              size="sm"
              className="min-w-24"
              onClick={copy}
            >
              {copied ? <Check /> : <Copy />}
              {copied ? "Copied" : "Copy script"}
            </Button>
          </div>
        </div>
      </section>
      {(error || status.error) && (
        <p role="alert" className="mb-3 text-xs text-destructive-foreground">
          {error || status.error}
        </p>
      )}
      <div className="mt-auto">
        <Button
          className="w-full"
          size="lg"
          disabled={retrying || status.starting || (!status.ready && !status.error)}
          onClick={() => status.error ? retryConnection() : action("minimize_to_tray")}
        >
          {retrying || status.starting ? <LoaderCircle className="animate-spin" /> : status.error ? <RotateCw /> : null}
          {retrying || status.starting ? "Starting…" : status.error ? "Retry connection" : status.ready ? "Run in background" : "Starting…"}
        </Button>
        <p className="mt-3 text-center text-xs text-muted-foreground">
          Reopen or exit from the system tray.
        </p>
      </div>
    </main>
  );
}
createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
