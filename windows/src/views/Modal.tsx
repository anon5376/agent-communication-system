import type { ComponentChildren } from "preact";
import { useEffect, useRef } from "preact/hooks";
import { store } from "../store";
import { focusBoundaryIndex } from "../modal-focus";

export function Modal(props: { title: string; width?: number; onClose: () => void; children: ComponentChildren }) {
  const ref = useRef<HTMLDialogElement>(null);
  const close = useRef(props.onClose);
  close.current = props.onClose;

  useEffect(() => {
    const dialog = ref.current;
    const previous = document.activeElement;
    dialog?.showModal();
    return () => {
      dialog?.close();
      if (previous instanceof HTMLElement && previous.isConnected) previous.focus();
    };
  }, []);

  return (
    <dialog
      ref={ref}
      class="sheet native-dialog"
      aria-label={props.title}
      style={{ width: props.width ?? 540 }}
      onKeyDown={(event) => {
        if (event.key !== "Tab") return;
        const controls = [...event.currentTarget.querySelectorAll<HTMLElement>(
          'button:not(:disabled), input:not(:disabled), textarea:not(:disabled), select:not(:disabled), a[href], summary, [tabindex="0"]',
        )].filter((element) => element.getClientRects().length > 0);
        const index = focusBoundaryIndex(controls.length, controls.indexOf(document.activeElement as HTMLElement), event.shiftKey);
        if (index !== null) {
          event.preventDefault();
          controls[index]?.focus();
        } else if (controls.length === 0) {
          event.preventDefault();
        }
      }}
      onCancel={(event) => {
        event.preventDefault();
        if (!store.busy.value) close.current();
      }}
    >
      {props.children}
    </dialog>
  );
}
