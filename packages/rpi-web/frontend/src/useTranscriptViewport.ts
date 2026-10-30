import { useCallback, useLayoutEffect, useRef, type RefObject } from "react";

/** Keep a visible row fixed while older rows or late image sizes change above it. */
export function useTranscriptViewport(
  transcript: RefObject<HTMLDivElement | null>,
  following: RefObject<boolean>,
  revision: string,
  ready: boolean,
  content: unknown,
  status: string,
) {
  const anchor = useRef<{ element: HTMLElement; offset: number } | null>(null);

  const remember = useCallback(() => {
    const node = transcript.current;
    if (!node || following.current) {
      anchor.current = null;
      return;
    }
    const top = node.getBoundingClientRect().top;
    const rows = node.querySelectorAll<HTMLElement>("[data-transcript-row]");
    // Rows are in document order; avoid measuring every loaded row on each scroll.
    let left = 0;
    let right = rows.length;
    while (left < right) {
      const middle = (left + right) >>> 1;
      if (rows[middle].getBoundingClientRect().bottom <= top + 1)
        left = middle + 1;
      else right = middle;
    }
    const element = rows[left];
    anchor.current = element
      ? { element, offset: element.getBoundingClientRect().top - top }
      : null;
  }, [transcript, following]);

  const restore = useCallback(() => {
    const node = transcript.current;
    if (!node) return;
    if (following.current) {
      anchor.current = null;
      node.scrollTop = node.scrollHeight - node.clientHeight;
      return;
    }
    const saved = anchor.current;
    if (!saved || !node.contains(saved.element)) return;
    const delta =
      saved.element.getBoundingClientRect().top -
      node.getBoundingClientRect().top -
      saved.offset;
    if (Math.abs(delta) > 0.5) node.scrollTop += delta;
  }, [transcript, following]);

  useLayoutEffect(() => {
    anchor.current = null;
  }, [revision]);

  useLayoutEffect(restore, [restore, content, status]);

  useLayoutEffect(() => {
    const list = transcript.current?.querySelector(".message-list");
    if (!ready || !list) return;
    // Images and expanded tool output can resize long after a page was prepended.
    const observer = new ResizeObserver(restore);
    observer.observe(list);
    return () => observer.disconnect();
  }, [transcript, restore, revision, ready]);

  return remember;
}
