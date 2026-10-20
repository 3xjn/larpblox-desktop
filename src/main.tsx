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
  browsers: number;
  games: number;
  startup: boolean;
  error: string | null;
};
function App() {
  const [status, setStatus] = useState<Status>({
    ready: false,
    browsers: 0,
    games: 0,
    startup: false,
    error: null,
  });
  const [error, setError] = useState("");
  const [copied, setCopied] = useState(false);
  const [saving, setSaving] = useState(false);
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
          <div className="p-4">
            <div className="flex items-center gap-3">
              <div className="flex size-8 items-center justify-center">
                <Chrome className="size-5 text-muted-foreground" />
              </div>
              <div className="flex-1 text-sm font-medium">Browser</div>
              <span
                role="status"
                className={`flex items-center gap-1.5 text-xs ${status.browsers ? "text-success-foreground" : "text-muted-foreground"}`}
              >
                {status.browsers > 0 && <Check className="size-3.5" />}
                {status.browsers ? "Connected" : "Not connected"}
              </span>
            </div>
            <div className="mt-3 flex items-center justify-between gap-2">
              <p className="max-w-52 text-xs leading-relaxed text-muted-foreground">
                {status.browsers
                  ? "Your extension is connected."
                  : "Open Roblox in Chrome."}
              </p>
              <Button
                variant="outline"
                size="sm"
                onClick={() => action("open_roblox")}
              >
                Open
                <ArrowUpRight />
              </Button>
            </div>
          </div>
          <Separator />
          <div className="p-4">
            <div className="flex items-center gap-3">
              <div className="flex size-8 items-center justify-center">
                <Gamepad2 className="size-5 text-muted-foreground" />
              </div>
              <div className="flex-1 text-sm font-medium">Game</div>
              <span
                role="status"
                className={`flex items-center gap-1.5 text-xs ${status.games ? "text-success-foreground" : "text-muted-foreground"}`}
              >
                {status.games > 0 && <Check className="size-3.5" />}
                {status.games ? "Connected" : "Not connected"}
              </span>
            </div>
            <div className="mt-3 flex items-center justify-between gap-2">
              <p className="max-w-52 text-xs leading-relaxed text-muted-foreground">
                {status.games
                  ? "Your game is ready to sync."
                  : "Run Larpblox in your game."}
              </p>
              <Button
                variant="outline"
                size="sm"
                className="min-w-24"
                onClick={copy}
              >
                {copied ? <Check /> : <Copy />}
                {copied ? "Copied" : "Copy script"}
              </Button>
            </div>
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
          disabled={!status.ready}
          onClick={() => action("minimize_to_tray")}
        >
          {!status.ready && <LoaderCircle className="animate-spin" />}
          {status.ready ? "Run in background" : "Starting…"}
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
