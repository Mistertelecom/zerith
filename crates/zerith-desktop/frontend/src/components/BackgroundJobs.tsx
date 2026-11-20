// Background shell jobs (`shell run_in_background`) still running, with a Stop
// button each. They live only while their turn runs (the turn owns the tool
// host), so the strip polls while a turn is busy and once on mount.
import { useCallback, useEffect, useState } from "react";
import { CircleStop, SquareTerminal } from "lucide-react";
import { jobsApi, type BackgroundJob, type Session } from "../lib/api";

function ago(secs: number): string {
  if (secs < 60) return `há ${secs} s`;
  if (secs < 3600) return `há ${Math.floor(secs / 60)} min`;
  return `há ${Math.floor(secs / 3600)} h`;
}

export function BackgroundJobs({
  active,
  selected,
  sessions,
}: {
  /** A turn is running somewhere: poll. */
  active: boolean;
  selected: string | null;
  sessions: Session[];
}) {
  const [jobs, setJobs] = useState<BackgroundJob[]>([]);
  const [stopping, setStopping] = useState<string | null>(null);
  const load = useCallback(() => {
    jobsApi
      .list()
      .then(setJobs)
      .catch(() => setJobs([]));
  }, []);

  useEffect(() => {
    load();
    if (!active) return;
    const t = setInterval(load, 2000);
    return () => clearInterval(t);
  }, [active, load]);

  if (!jobs.length) return null;
  return (
    <section className="jobs-strip" aria-label="Processos em segundo plano">
      {jobs.map((j) => {
        const other =
          j.session && j.session !== selected
            ? (sessions.find((s) => s.id === j.session)?.title ?? "subagente")
            : null;
        return (
          <div className="job-row" key={j.id}>
            <span className="job-icon" aria-hidden>
              <SquareTerminal size={14} strokeWidth={2} />
            </span>
            <code className="job-cmd" title={j.command}>
              {j.command}
            </code>
            <span className="job-meta">
              <span className="dot accent live" aria-hidden />
              rodando {ago(j.secs)} · {j.id}
              {other ? ` · ${other}` : ""}
            </span>
            <button
              className="btn btn-sm btn-danger"
              disabled={stopping === j.id}
              aria-label={`Parar ${j.id}: ${j.command}`}
              title="Encerra o processo e tudo que ele iniciou"
              onClick={() => {
                setStopping(j.id);
                jobsApi
                  .stop(j.id)
                  .catch(() => undefined)
                  .finally(() => {
                    setStopping(null);
                    load();
                  });
              }}
            >
              <CircleStop size={14} strokeWidth={2} />
              Parar
            </button>
          </div>
        );
      })}
    </section>
  );
}
