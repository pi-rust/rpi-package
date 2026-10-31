import { useCallback, useEffect, useRef, useState } from "react";
import type { SeqEvent } from "./api";
import {
  applyTraceEvent,
  applyTraceSnapshot,
  createExecutionTrace,
  type ExecutionTraceState,
} from "./executionTrace";

type Cell = { binding: string; state: ExecutionTraceState; frame?: number };

/** Publications are batched; mutations of the ref are synchronous and ordered. */
export function useExecutionTrace(binding: string) {
  const cell = useRef<Cell>({ binding, state: createExecutionTrace() });
  const mounted = useRef(false);
  // Render-time isolation matters even before effects for A -> B -> A navigation.
  if (cell.current.binding !== binding)
    cell.current = { binding, state: createExecutionTrace() };
  const [published, setPublished] = useState(() => ({
    owner: cell.current,
    state: cell.current.state,
  }));

  const publish = useCallback(() => {
    const owner = cell.current;
    if (!mounted.current || owner.frame !== undefined) return;
    owner.frame = requestAnimationFrame(() => {
      owner.frame = undefined;
      if (mounted.current && cell.current === owner)
        setPublished({ owner, state: owner.state });
    });
  }, []);
  const snapshot = useCallback(
    (
      frame: Parameters<typeof applyTraceSnapshot>[1],
      mode: Parameters<typeof applyTraceSnapshot>[2],
    ) => {
      cell.current.state = applyTraceSnapshot(cell.current.state, frame, mode);
      publish();
    },
    [publish],
  );
  const event = useCallback(
    (value: SeqEvent, seq: number) => {
      cell.current.state = applyTraceEvent(cell.current.state, value, seq);
      publish();
    },
    [publish],
  );
  const clear = useCallback(() => {
    cell.current.state = createExecutionTrace();
    publish();
  }, [publish]);

  useEffect(() => {
    const owner = cell.current;
    mounted.current = true;
    // Includes calls before mount and the second StrictMode effect setup.
    publish();
    return () => {
      mounted.current = false;
      if (owner.frame !== undefined) cancelAnimationFrame(owner.frame);
      owner.frame = undefined;
    };
  }, [binding, publish]);

  return {
    state:
      published.owner === cell.current ? published.state : cell.current.state,
    snapshot,
    event,
    clear,
  };
}
