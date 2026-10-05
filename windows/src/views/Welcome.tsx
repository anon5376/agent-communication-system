import { Icon } from "../icons";
import { store } from "../store";

export function Welcome() {
  return (
    <div class="welcome">
      <div class="welcome-inner">
        <div class="mark">
          <Icon name="logo" size={52} hidden />
        </div>
        <div>
          <h1>{"Different agents.\nOne place to work."}</h1>
          <p class="lede">
            Give your coding agents a task, see who owns it, and review what
            comes back. No terminal needed to manage the work.
          </p>
        </div>
        <div class="welcome-steps">
          <Step n="1" title="Choose a project" sub="Keep its tasks, messages, and review history together." />
          <Step n="2" title="Connect your agents" sub="Use coding CLIs you already have. You choose when they run." />
          <Step n="3" title="Give work a clear finish line" sub="Write the task and acceptance criteria, then review the result." />
        </div>
        <div class="cta-row">
          <button class="btn primary large" onClick={() => void store.chooseProject()} disabled={store.busy.value}>
            Open a Project…
          </button>
          <button class="btn large" onClick={() => void store.openSample()} disabled={store.busy.value}>
            Try the Sample
          </button>
        </div>
        <p class="fine">The sample is free and simulated. No account, API key, or model call.</p>
        <button class="link-btn" onClick={() => void store.chooseDatabase()} disabled={store.busy.value}>
          Already use ACS? Connect an existing bus…
        </button>
      </div>
    </div>
  );
}

function Step(props: { n: string; title: string; sub: string }) {
  return (
    <div class="wstep">
      <div class="num">{props.n}</div>
      <div>
        <div class="t">{props.title}</div>
        <div class="s">{props.sub}</div>
      </div>
    </div>
  );
}
