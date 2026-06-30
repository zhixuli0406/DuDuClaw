/** Three bouncing dots shown while the assistant is streaming a reply. */
export function TypingIndicator() {
  return (
    <div className="flex justify-start">
      <div className="flex items-center gap-1 rounded-2xl border border-[var(--panel-border)] bg-[var(--panel-fill)] px-4 py-3">
        <span className="h-2 w-2 animate-bounce rounded-full bg-stone-400 [animation-delay:0ms]" />
        <span className="h-2 w-2 animate-bounce rounded-full bg-stone-400 [animation-delay:150ms]" />
        <span className="h-2 w-2 animate-bounce rounded-full bg-stone-400 [animation-delay:300ms]" />
      </div>
    </div>
  );
}
