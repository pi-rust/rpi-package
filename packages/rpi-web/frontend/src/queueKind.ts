/**
 * The native host reports a queued entry's kind with `QueueKind::as_str()`,
 * where the next-run queue serialises as `nextRun`. `rpc_queue_message` only
 * accepts `nextTurn | next_run` for that queue, so re-submitting a reported
 * kind verbatim fails with `unknown queue kind: nextRun` after the original
 * entry was already cancelled. Map every reported spelling to the delivery
 * value the host accepts.
 */
const deliverAs: Record<string, string> = {
  steer: "steer",
  followUp: "followUp",
  follow_up: "followUp",
  nextTurn: "nextTurn",
  nextRun: "nextTurn",
  next_run: "nextTurn",
};

/**
 * The `deliverAs` value for a reported queue kind, or `null` when the entry
 * cannot be re-queued through `queue_message` (for example `context`, which
 * native code commits at a turn boundary and never accepts as a delivery).
 */
export function queueDeliverAs(kind: string): string | null {
  return deliverAs[kind] ?? null;
}
