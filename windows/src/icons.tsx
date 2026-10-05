// Minimal inline icon set covering the SF Symbols used by the macOS app,
// drawn in a neutral outline style that reads natively on Windows.

import type { JSX } from "preact";

const PATHS: Record<string, JSX.Element> = {
  // point.3.connected.trianglepath.dotted — the ACS mark: three nodes, dotted triangle
  logo: (
    <g>
      <circle cx="12" cy="5" r="2.6" />
      <circle cx="5" cy="18" r="2.6" />
      <circle cx="19" cy="18" r="2.6" />
      <path
        d="M9.7 16.4 6.9 7.4M14.3 16.4l2.8-9M9.6 18h4.8"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.7"
        strokeLinecap="round"
        strokeDasharray="1 3"
      />
    </g>
  ),
  checklist: (
    <g>
      <path d="M5 6.5 6.5 8 9.5 4.5" />
      <path d="M5 13l1.5 1.5 3-3.5" />
      <path d="M5 19.5l1.5 1.5 3-3.5" />
      <path d="M12 6.5h8M12 13h8M12 19.5h8" />
    </g>
  ),
  tray: (
    <g>
      <path d="M3.5 13.5h4.7l1.4 2.2h4.8l1.4-2.2h4.7" />
      <path d="M5.5 5.5h13l2 8H3.5l2-8Z" />
      <path d="M3.5 13.5v5h17v-5" />
    </g>
  ),
  "tray-full": (
    <g>
      <path d="M3.5 13.5h4.7l1.4 2.2h4.8l1.4-2.2h4.7" />
      <path d="M5.5 9h13l2 4.5H3.5l2-4.5Z" />
      <path d="M6.8 5.5h10.4M3.5 13.5v5h17v-5" />
    </g>
  ),
  people: (
    <g>
      <circle cx="9" cy="8" r="3" />
      <path d="M3.5 19.5c.6-3.2 2.8-5 5.5-5s4.9 1.8 5.5 5" />
      <circle cx="16.5" cy="9" r="2.4" />
      <path d="M16 14.7c2.3.3 4 1.9 4.5 4.3" />
    </g>
  ),
  chat: (
    <g>
      <path d="M4 5.5h11.5v8H9l-3.4 2.8v-2.8H4v-8Z" />
      <path d="M18.5 9.5H20v6.6h-1.4v2.2L16 16h-2.6" />
    </g>
  ),
  sparkles: (
    <g>
      <path d="M12 4l1.6 4.4L18 10l-4.4 1.6L12 16l-1.6-4.4L6 10l4.4-1.6L12 4Z" />
      <path d="M18.5 15.5l.8 2.2 2.2.8-2.2.8-.8 2.2-.8-2.2-2.2-.8 2.2-.8.8-2.2Z" />
    </g>
  ),
  folder: (
    <path d="M3.5 6.5h5.2l1.8 2h10v10h-17v-12Z" />
  ),
  drive: (
    <g>
      <rect x="3.5" y="9" width="17" height="7" rx="1.5" />
      <circle cx="17" cy="12.5" r="0.9" fill="currentColor" stroke="none" />
      <path d="M7 12.5h6" />
    </g>
  ),
  xmark: <path d="M6 6l12 12M18 6L6 18" />,
  checkmark: <path d="M5 12.5l4.5 4.5L19 7.5" />,
  "checkmark-circle": (
    <g>
      <circle cx="12" cy="12" r="8.5" />
      <path d="M8 12.4l2.8 2.8L16.5 9" />
    </g>
  ),
  "exclamation-triangle": (
    <g>
      <path d="M12 4 2.8 19.5h18.4L12 4Z" />
      <path d="M12 10v4.5" />
      <circle cx="12" cy="17" r="0.9" fill="currentColor" stroke="none" />
    </g>
  ),
  "exclamation-circle": (
    <g>
      <circle cx="12" cy="12" r="8.5" />
      <path d="M12 7.5v6" />
      <circle cx="12" cy="16.8" r="0.9" fill="currentColor" stroke="none" />
    </g>
  ),
  circle: <circle cx="12" cy="12" r="8" />,
  "circle-dotted": (
    <circle
      cx="12"
      cy="12"
      r="8"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.7"
      strokeDasharray="2.5 3"
    />
  ),
  "arrow-uturn": (
    <g>
      <path d="M7.5 9.5 4 13l3.5 3.5" />
      <path d="M4 13h10.5a5 5 0 0 1 0 10H13" transform="translate(0,-2)" />
    </g>
  ),
  "slash-circle": (
    <g>
      <circle cx="12" cy="12" r="8.5" />
      <path d="M6.5 17.5 17.5 6.5" />
    </g>
  ),
  person: (
    <g>
      <rect x="4.5" y="4.5" width="15" height="15" rx="3" />
      <circle cx="12" cy="10" r="2.4" />
      <path d="M8 16.5c.7-2 2.2-3 4-3s3.3 1 4 3" />
    </g>
  ),
  "minus-circle": (
    <g>
      <circle cx="12" cy="12" r="8.5" />
      <path d="M8 12h8" />
    </g>
  ),
  refresh: (
    <g>
      <path d="M19 12a7 7 0 1 1-2-4.9" />
      <path d="M19 4v4h-4" />
    </g>
  ),
  "sidebar-right": (
    <g>
      <rect x="3.5" y="5" width="17" height="14" rx="2" />
      <path d="M15.5 5v14" />
    </g>
  ),
  plus: <path d="M12 5v14M5 12h14" />,
  lock: (
    <g>
      <rect x="6" y="10.5" width="12" height="9" rx="1.5" />
      <path d="M8.5 10.5V8a3.5 3.5 0 0 1 7 0v2.5" />
    </g>
  ),
};

export function Icon(props: {
  name: keyof typeof PATHS | string;
  size?: number;
  class?: string;
  label?: string;
  hidden?: boolean;
}) {
  const size = props.size ?? 18;
  return (
    <svg
      class={props.class}
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill={props.name === "logo" || props.name === "sparkles" ? "currentColor" : "none"}
      stroke="currentColor"
      strokeWidth="1.7"
      strokeLinecap="round"
      strokeLinejoin="round"
      role={props.label ? "img" : undefined}
      aria-label={props.label}
      aria-hidden={props.hidden || !props.label ? "true" : undefined}
      focusable="false"
    >
      {PATHS[props.name] ?? PATHS["circle"]}
    </svg>
  );
}
