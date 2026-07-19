/**
 * The selected work item's header — always work-first, never subsystem-first.
 *
 * Shows what the user selected (an env or a team run), where it is in its
 * lifecycle (draft → working → review → applied / decided), who sits on it,
 * and how much unseen attention points at it. Rendered above whichever lens
 * (Sandbox, Ensemble, …) is displaying the entity, so the subject stays
 * fixed while the lens changes.
 */
import { Icon, Tag } from "@blueprintjs/core";
import type { WorkItem } from "./attention";

const ENV_STEPS = ["draft", "working", "review", "applied"];
const TEAM_STEPS = ["collect", "review", "decided"];

function Stepper({ lifecycle, kind }: { lifecycle: string; kind: string }) {
  const steps = kind === "team" ? TEAM_STEPS : ENV_STEPS;
  const idx = steps.indexOf(lifecycle);
  if (idx === -1) {
    // Off-path states (aborted, engine-specific phases) are shown honestly
    // as themselves rather than forced onto the happy path.
    return (
      <span className={`wi-step wi-step-now${lifecycle === "aborted" ? " wi-step-bad" : ""}`}>
        {lifecycle}
      </span>
    );
  }
  return (
    <>
      {steps.map((s, i) => (
        <span
          key={s}
          className={`wi-step ${i < idx ? "wi-step-done" : i === idx ? "wi-step-now" : ""}`}
        >
          {s}
        </span>
      ))}
    </>
  );
}

export function WorkItemStrip({ item }: { item: WorkItem }) {
  return (
    <div className="wi-strip">
      <Icon icon={item.kind === "team" ? "people" : "shield"} size={13} />
      <span className="wi-title">{item.title}</span>
      <span className="wi-id">{item.id}</span>
      <span className="wi-steps">
        <Stepper lifecycle={item.lifecycle} kind={item.kind} />
      </span>
      <span className="wi-seats">
        {item.seats.map((s) => (
          <span key={s.agent} className={`wi-seat wi-seat-${s.status}`} title={s.env_id}>
            {s.agent}
          </span>
        ))}
      </span>
      {item.unseen > 0 ? (
        <Tag minimal intent="warning" title="Unseen attention items on this work item">
          {item.unseen} unseen
        </Tag>
      ) : null}
    </div>
  );
}
