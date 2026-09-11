import type { Fleet } from "./App";
import { Cmd, Empty, Facts, Note, SectionHead, Spin } from "./ui";

/** What this machine can enforce. Read once: the probe shells out. */
export function HostPage({ fleet }: { fleet: Fleet }) {
  const p = fleet.probe;
  return (
    <>
      <div className="topbar">
        <span className="topbar-title">Host</span>
        <span className="topbar-scope">what a box on this machine can actually be held to</span>
      </div>
      <div className="page scroll">
        {p === null ? (
          <Spin label="probing the host" />
        ) : (
          <div className="host">
            <Note tone={p.strongest_tier === "workspace" ? "warn" : "plain"}>
              The strongest tier this host can run is <code>{p.strongest_tier}</code>, by{" "}
              <code>{p.mechanism}</code>. A tier that resolves but will not run is not a green tick:
              each claim below carries the functional self-test, not just the policy check.
            </Note>

            <div>
              <SectionHead>Isolation tiers</SectionHead>
              <div className="claims">
                {p.claims.map((c) => {
                  const ok = c.satisfiable && c.runnable !== false;
                  return (
                    <div key={c.claim} className={`claim ${ok ? "is-ok" : "is-off"}`}>
                      <b>{c.claim}</b>
                      <span>
                        {c.note ??
                          (c.runnable === false
                            ? "the policy resolves here, but a confined exec fails"
                            : ok
                              ? "resolves and runs"
                              : "not available on this host")}
                      </span>
                    </div>
                  );
                })}
              </div>
            </div>

            <div>
              <SectionHead>Boundaries</SectionHead>
              <div className="claims">
                <div className={`claim ${p.egress_enforced ? "is-ok" : "is-off"}`}>
                  <b>egress allowlist</b>
                  <span>
                    {p.egress_enforced
                      ? "a domain allowlist can be enforced here"
                      : "no allowlist enforcement: the kernel tiers deny all or allow all"}
                  </span>
                </div>
                <div className={`claim ${p.resource_limits ? "is-ok" : "is-off"}`}>
                  <b>resource limits</b>
                  <span>
                    {p.memory_limit
                      ? "cpu, procs, wall and memory limits enforceable"
                      : "no memory cap on this host"}
                  </span>
                </div>
                <div className={`claim ${p.syscall_filter ? "is-ok" : "is-off"}`}>
                  <b>syscall filter</b>
                  <span>{p.syscall_filter ? "seccomp is available" : "no seccomp"}</span>
                </div>
              </div>
            </div>

            <div>
              <SectionHead>Mechanisms</SectionHead>
              <Facts
                rows={[
                  ["os", p.os],
                  ["landlock", p.landlock_abi != null ? `ABI ${p.landlock_abi}` : "absent"],
                  ["user namespaces", p.userns ? "yes" : "no"],
                  ["seccomp", p.seccomp ? "yes" : "no"],
                  ["seatbelt", p.seatbelt ? "yes" : "no"],
                  ["container runtime", p.container_runtime ?? "none found"],
                ]}
              />
            </div>

            <Empty title="The same report, from the terminal">
              <Cmd text="h5i box capabilities" hint="the probe this page shows" />
              <Cmd text="h5i box capabilities --json" />
            </Empty>
          </div>
        )}
      </div>
    </>
  );
}
