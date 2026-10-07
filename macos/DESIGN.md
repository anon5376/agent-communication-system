# ACS for macOS

A native operator workspace, complementary to `aos` and `qagent`. The visual language is macOS, not a terminal inside a window: system typography, semantic colors, familiar sidebar navigation, split task list and inspector, native sheets and file choosers. Respect light/dark appearance, keyboard navigation and selectable text.

The first screen explains the task → owner → submission → review workflow. Its primary action opens a project; the clearly simulated sample is the safe secondary action. Normal workspaces start empty. Sample state stays visible across every destination.

Tasks show human decisions first, with pending review above the queue. The inspector owns the complete brief, acceptance criteria, file scope, submission, reported checks, notes, and review. No invented progress percentages, cost figures or readiness claims. Color reinforces text and symbols, never replaces them.

Agent controls require an explicit project choice and consent before starting real providers. Installed, authenticated, bus presence and running supervisor are different states. Mutations refresh from the Rust source of truth; errors remain actionable and visible rather than silently changing local state.

Layout targets 1180 × 780, with a 960 × 640 minimum, resizable sidebar and task panes, scrolling content and multiline error banners. Native controls retain platform focus rings and accessibility labels. A task creation sheet requires a brief and acceptance criteria and prevents selecting the same named worker and reviewer.
