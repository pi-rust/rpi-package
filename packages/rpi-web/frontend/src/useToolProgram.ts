import { useEffect, useRef, useState } from "react";
import { api, type ToolProgramStreamItem } from "./api";
import {
  applyChildUpdate,
  childUnsupported,
  readChildFeed,
  seedChildSnapshot,
  type ToolProgramView,
} from "./toolProgram";

export function useToolProgram(
  sessionId: string,
  enabled: boolean,
  revision: number,
) {
  const [view, setView] = useState<{
    snapshot: ToolProgramView | null;
    status: "loading" | "live" | "reconnecting" | "unsupported" | "error";
    error?: string;
    generation: number;
  }>({ snapshot: null, status: "loading", generation: 0 });
  const generation = useRef(0);
  useEffect(() => {
    if (!enabled || !sessionId) return;
    const owner = new AbortController();
    let feed: AbortController | null = null;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let current: ToolProgramView | null = null;
    let retries = 0;
    const accept = (raw: unknown) => {
      current = seedChildSnapshot(raw, sessionId);
      setView({
        snapshot: current,
        status: "live",
        generation: ++generation.current,
      });
    };
    async function connect() {
      // Retire old feed BEFORE any fresh query; no transcript cursor is used.
      feed?.abort();
      feed = new AbortController();
      const controller = feed;
      const abort = () => controller.abort();
      owner.signal.addEventListener("abort", abort, { once: true });
      setView({
        snapshot: null,
        status: retries ? "reconnecting" : "loading",
        generation: ++generation.current,
      });
      try {
        const initial = await api.toolProgramSnapshot(
          encodeURIComponent(sessionId),
          controller.signal,
        );
        if (owner.signal.aborted) return;
        accept(initial);
        let receivedSnapshot = false;
        await readChildFeed(sessionId, controller.signal, (name, value) => {
          if (owner.signal.aborted) return;
          if (name === "tool_program_child_snapshot") {
            accept(value); // This subscription's own gap-checked boundary wins.
            receivedSnapshot = true;
          } else if (name === "tool_program_child_update") {
            if (!receivedSnapshot || !current)
              throw new Error("Child update before snapshot");
            current = applyChildUpdate(current, value as ToolProgramStreamItem);
            setView((previous) => ({
              ...previous,
              snapshot: current,
              status: "live",
            }));
          } else throw new Error("Unexpected child stream frame");
        });
      } catch (error) {
        controller.abort();
        if (owner.signal.aborted) return;
        if (childUnsupported(error)) {
          setView({
            snapshot: null,
            status: "unsupported",
            generation: ++generation.current,
          });
          return;
        }
        const message =
          error instanceof Error ? error.message : "Child inspection failed";
        // Bounded recovery. HTTP/auth failures never become empty children.
        const retry =
          retries++ < 2 &&
          !(error && typeof error === "object" && "status" in error);
        setView({
          snapshot: null,
          status: retry ? "reconnecting" : "error",
          error: message,
          generation: ++generation.current,
        });
        if (retry) timer = setTimeout(() => void connect(), 1000 * retries);
      } finally {
        owner.signal.removeEventListener("abort", abort);
      }
    }
    void connect();
    return () => {
      owner.abort();
      feed?.abort();
      if (timer) clearTimeout(timer);
    };
  }, [sessionId, enabled, revision]);
  return view;
}
