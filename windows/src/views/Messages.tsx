// MessagesView — latest 100 workspace messages + compose sheet, matching
// MessagesView.swift's sorting, copy and validation.

import { useState } from "preact/hooks";
import { EmptyState } from "../App";
import { timestamp, type MessageRecord } from "../model";
import { store } from "../store";

export function MessagesView() {
  const [composing, setComposing] = useState(false);
  const messages = [...(store.snapshot.value?.messages ?? [])].sort(
    (a, b) => Number(b.seq) - Number(a.seq),
  );

  return (
    <div style={{ display: "flex", flexDirection: "column", flex: 1, minHeight: 0 }}>
      <div class="row-between" style={{ padding: "26px 30px 18px" }}>
        <div>
          <h1 class="view-title">Messages</h1>
          <p class="view-sub" style={{ maxWidth: 620 }}>
            The latest 100 messages across this workspace. Reading here does not acknowledge an
            agent’s inbox.
          </p>
        </div>
        <span style={{ flex: 1 }} />
        <button class="btn" onClick={() => setComposing(true)} disabled={!store.canWrite.value}>
          New Message…
        </button>
      </div>
      <hr class="divider" />
      {messages.length === 0 ? (
        <EmptyState
          icon="chat"
          title="No messages yet"
          message="Agent handoffs, questions, and feedback will appear here."
        />
      ) : (
        <div class="view-scroll">
          <div style={{ padding: "0 30px" }}>
            {messages.map((message) => (
              <MessageRow key={String(message.seq)} message={message} />
            ))}
          </div>
        </div>
      )}
      {composing && <ComposeMessageSheet onClose={() => setComposing(false)} />}
    </div>
  );
}

export function MessageRow(props: { message: MessageRecord }) {
  const m = props.message;
  return (
    <div class="msg-row">
      <div class="m-head">
        <span>
          {m.sender} → {m.recipient ?? "Everyone"}
        </span>
        <span class="m-time">{timestamp(m.tsMs)}</span>
      </div>
      <div class="m-subject">{m.subject}</div>
      <div class="m-body">{m.body}</div>
    </div>
  );
}

function ComposeMessageSheet(props: { onClose: () => void }) {
  const [to, setTo] = useState("*");
  const [subject, setSubject] = useState("");
  const [body, setBody] = useState("");
  const [failure, setFailure] = useState<string | null>(null);
  const agents = store.agents.value;
  const busy = store.busy.value;

  const send = async () => {
    const ok = await store.mutate("send", { to, subject, body });
    if (ok) props.onClose();
    else setFailure(store.error.value ?? "The message could not be sent.");
  };

  return (
    <div
      class="sheet-scrim"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget && !busy) props.onClose();
      }}
      onKeyDown={(e) => {
        if (e.key === "Escape" && !busy) props.onClose();
      }}
    >
      <div class="sheet" role="dialog" aria-modal="true" style={{ width: 540 }}>
        <h2>New message</h2>
        <div class="field">
          <span class="f-label">To</span>
          <select value={to} onChange={(e) => setTo((e.target as HTMLSelectElement).value)} aria-label="Recipient">
            <option value="*">Everyone (broadcast)</option>
            {agents.map((a) => (
              <option value={a.id}>{a.id}</option>
            ))}
          </select>
        </div>
        <div class="field">
          <span class="f-label">Subject</span>
          <input
            type="text"
            value={subject}
            onInput={(e) => setSubject((e.target as HTMLInputElement).value)}
            aria-label="Subject"
          />
        </div>
        <div class="field">
          <textarea
            value={body}
            onInput={(e) => setBody((e.target as HTMLTextAreaElement).value)}
            style={{ minHeight: 170 }}
            aria-label="Message body"
          />
        </div>
        <p class="inline-note">
          Messages may wake an already running agent. They do not start stopped agents.
        </p>
        {failure && <p class="form-error">{failure}</p>}
        <div class="actions">
          <span class="spacer" />
          <button class="btn" onClick={props.onClose} disabled={busy}>
            Cancel
          </button>
          <button
            class="btn primary"
            onClick={() => void send()}
            disabled={!store.canWrite.value || subject.trim() === "" || body.trim() === ""}
          >
            Send Message
          </button>
        </div>
      </div>
    </div>
  );
}
