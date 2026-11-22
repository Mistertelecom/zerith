// Background shell jobs (`shell run_in_background`) still running, with a Stop
// button each. A job outlives its turn (a GUI script keeps its window after the
// answer), so the strip polls while one exists, not only while a turn runs.
import { useCallback, useEffect, useState } from "react";
import { ChevronRight, CircleStop, SquareTerminal } from "lucide-react";
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
  const [shown, setShown] = useState<string | null>(null);
  const [tail, setTail] = useState("");

  // The open job's log follows while it is open.
  useEffect(() => {
    if (!shown) return;
    let live = true;
    const pull = () =>
      void jobsApi
        .tail(shown)
        .then((t) => live && setTail(t))
        .catch(() => live && setShown(null));
    pull();
    const t = setInterval(pull, 1500);
    return () => {
      live = false;
      clearInterval(t);
    };
  }, [shown]);
  const load = useCallback(() => {
    jobsApi
      .list()
      .then(setJobs)
      .catch(() => setJobs([]));
  }, []);

  useEffect(() => {
    load();
    const t = setInterval(load, active || jobs.length ? 2000 : 8000);
    return () => clearInterval(t);
  }, [active, jobs.length > 0, load]);

  if (!jobs.length) return null;
  return (
    <section className="jobs-strip" aria-label="Processos em segundo plano">
      {jobs.map((j) => {
        const other =
          j.session && j.session !== selected
            ? (sessions.find((s) => s.id === j.session)?.title ?? "subagente")
            : null;
        return (
          <div className="job" key={j.id}>
          <div className="job-row">
            <button
              type="button"
              className={"job-toggle" + (shown === j.id ? " on" : "")}
              aria-expanded={shown === j.id}
              aria-label={`Ver saída de ${j.id}`}
              title="Ver a saída"
              onClick={() => {
                setTail("");
                setShown(shown === j.id ? null : j.id);
              }}
            >
              <ChevronRight size={14} strokeWidth={2} className="chev" />
              <SquareTerminal size={14} strokeWidth={2} aria-hidden />
            </button>
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
          {shown === j.id ? <pre className="job-tail">{tail || "(sem saída ainda)"}</pre> : null}
          </div>
        );
      })}
    </section>
  );
}
