/**
 * One request id per user action, kept until the server answers.
 *
 * A retry of the same action (after a timeout or a lost reply) sends the same
 * id, so the server can reconnect it to the run it already started instead of
 * starting a second one. A new id is made only after the previous action got
 * an answer, success or a stated refusal. Used by "run now" on routines, which the gateway requires for a
 * routine bound to a workflow.
 */
export function createRequestIdKeeper(newId: () => string = () => crypto.randomUUID()) {
  const pending = new Map<string, string>();
  return {
    /** The id for this action on `key`: the pending one, or a fresh one. */
    take(key: string): string {
      const existing = pending.get(key);
      if (existing) return existing;
      const id = newId();
      pending.set(key, id);
      return id;
    },
    /** The server answered; the next action on `key` is a new one. */
    settle(key: string): void {
      pending.delete(key);
    },
  };
}

/**
 * `true` when a rejected request was answered by the server (an error
 * response), as opposed to a timeout or a lost connection. The gateway
 * client rejects transport failures with `Error` objects and server error
 * responses with the response's error value.
 */
export function serverAnswered(rejection: unknown): boolean {
  return rejection !== undefined && rejection !== null && !(rejection instanceof Error);
}
