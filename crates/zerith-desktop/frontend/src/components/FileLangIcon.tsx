/** Crisp inline SVG file-type & language icons matching Antigravity / VS Code style. */
export function FileLangBadge({ path }: { path: string }) {
  const base = (path.split(/[\\/]/).pop() ?? path).toLowerCase();
  const ext = base.includes(".") ? (base.split(".").pop() ?? "") : base;

  const svg = renderIcon(base, ext);
  return (
    <span className="lang-svg" title={base} aria-hidden>
      {svg}
    </span>
  );
}

function renderIcon(base: string, ext: string) {
  if (base === "dockerfile" || base.startsWith("dockerfile.")) {
    return (
      <svg viewBox="0 0 24 24" width="15" height="15">
        <rect x="4" y="10" width="3.5" height="3.5" rx="0.5" fill="#2496ED" />
        <rect x="8.5" y="10" width="3.5" height="3.5" rx="0.5" fill="#2496ED" />
        <rect x="13" y="10" width="3.5" height="3.5" rx="0.5" fill="#2496ED" />
        <rect x="8.5" y="5.5" width="3.5" height="3.5" rx="0.5" fill="#2496ED" />
        <rect x="13" y="5.5" width="3.5" height="3.5" rx="0.5" fill="#2496ED" />
        <path d="M2.5 14.5h17.5c1 0 2 .6 2 1.8 0 3-3.2 5.2-8.5 5.2-6.2 0-11-2.8-11-7z" fill="#1D63ED" />
      </svg>
    );
  }

  switch (ext) {
    case "rs":
      return (
        <svg viewBox="0 0 24 24" width="15" height="15">
          <circle
            cx="12"
            cy="12"
            r="8.5"
            fill="none"
            stroke="#EA580C"
            strokeWidth="2.4"
            strokeDasharray="2.3 1.7"
          />
          <circle cx="12" cy="12" r="7.3" fill="rgba(234,88,12,0.14)" stroke="#F97316" strokeWidth="1.2" />
          <path
            d="M8.5 16.5v-9h4.1c1.7 0 2.9.9 2.9 2.5 0 1.2-.7 2-1.8 2.3l2.1 4.2h-2.1l-1.9-3.9h-1.4v3.9H8.5zm1.9-5.4h2c.8 0 1.2-.4 1.2-1.1s-.4-1.1-1.2-1.1h-2v2.2z"
            fill="#FB923C"
          />
        </svg>
      );

    case "py":
      return (
        <svg viewBox="0 0 24 24" width="15" height="15">
          <path
            d="M11.9 2C7.8 2 8.1 3.8 8.1 3.8l.01 1.85h3.9v.55H6.6S4 5.9 4 10.1s2.3 4.05 2.3 4.05h1.35v-1.95s-.07-2.3 2.26-2.3h3.9s2.2.04 2.2-2.12V4.15S16.34 2 11.9 2zm-2.15 1.25a.7.7 0 1 1 0 1.4.7.7 0 0 1 0-1.4z"
            fill="#3776AB"
          />
          <path
            d="M12.1 22c4.1 0 3.8-1.8 3.8-1.8l-.01-1.85h-3.9v-.55h5.41S20 18.1 20 13.9s-2.3-4.05-2.3-4.05h-1.35v1.95s.07 2.3-2.26 2.3h-3.9s-2.2-.04-2.2 2.12v3.63S7.66 22 12.1 22zm2.15-1.25a.7.7 0 1 1 0-1.4.7.7 0 0 1 0 1.4z"
            fill="#FFD43B"
          />
        </svg>
      );

    case "ts":
      return (
        <svg viewBox="0 0 24 24" width="15" height="15">
          <rect x="2" y="2" width="20" height="20" rx="3.2" fill="#3178C6" />
          <path
            d="M6.2 12.5h2.7v6.6h1.9v-6.6h2.7V11H6.2v1.5zm12.3 4.1c0-1.1-.7-1.7-2.2-2.2-.9-.3-1.3-.5-1.3-1 0-.4.4-.7 1-.7.6 0 1.1.2 1.6.6l.8-1.3c-.6-.5-1.5-.8-2.4-.8-1.7 0-2.8.9-2.8 2.2 0 1.2.8 1.8 2.3 2.3.9.3 1.2.5 1.2 1 0 .5-.5.8-1.2.8-.8 0-1.5-.3-2-.8l-.9 1.3c.7.6 1.7 1 2.9 1 1.9 0 3-.9 3-2.4z"
            fill="#FFFFFF"
          />
        </svg>
      );

    case "tsx":
    case "jsx":
      return (
        <svg viewBox="0 0 24 24" width="15" height="15">
          <circle cx="12" cy="12" r="2.1" fill="#61DAFB" />
          <g fill="none" stroke="#61DAFB" strokeWidth="1.45">
            <ellipse cx="12" cy="12" rx="9.5" ry="3.8" />
            <ellipse cx="12" cy="12" rx="9.5" ry="3.8" transform="rotate(60 12 12)" />
            <ellipse cx="12" cy="12" rx="9.5" ry="3.8" transform="rotate(120 12 12)" />
          </g>
        </svg>
      );

    case "js":
    case "mjs":
    case "cjs":
      return (
        <svg viewBox="0 0 24 24" width="15" height="15">
          <rect x="2" y="2" width="20" height="20" rx="3.2" fill="#F7DF1E" />
          <path
            d="M12.2 17.5c0 1.3-.8 2-2.2 2-.9 0-1.6-.4-2.1-1l1.2-1c.3.4.5.6.9.6.5 0 .7-.2.7-.9v-5.7h1.5v6zm6.5-.2c0-1.1-.6-1.7-2-2.2-.8-.3-1.2-.5-1.2-1 0-.4.3-.7.9-.7.5 0 .9.2 1.3.5l.8-1.2c-.6-.4-1.3-.7-2.2-.7-1.6 0-2.6.9-2.6 2.1 0 1.2.8 1.8 2.1 2.2.8.3 1.1.5 1.1 1 0 .5-.4.8-1 .8-.7 0-1.3-.3-1.8-.8l-.8 1.2c.6.6 1.5 1 2.7 1 1.7 0 2.7-.9 2.7-2.2z"
            fill="#111827"
          />
        </svg>
      );

    case "go":
      return (
        <svg viewBox="0 0 24 24" width="15" height="15">
          <path
            d="M9.6 11.4H5.3v1.8h2.5c-.3 1.2-1.3 2-2.6 2-1.7 0-2.8-1.3-2.8-3.2s1.1-3.2 2.8-3.2c1.1 0 1.9.5 2.4 1.3l1.7-1.1C8.4 7.7 7 7 5.2 7 2.4 7 .4 9.1.4 12s2 5 4.8 5c2.9 0 4.6-2 4.6-4.9v-.7h-.2zm7.9-4.4c-3 0-5.2 2.1-5.2 5s2.2 5 5.2 5 5.2-2.1 5.2-5-2.2-5-5.2-5zm0 8.1c-1.8 0-3.1-1.3-3.1-3.1s1.3-3.1 3.1-3.1 3.1 1.3 3.1 3.1-1.3 3.1-3.1 3.1z"
            fill="#00ADD8"
          />
        </svg>
      );

    case "json":
      return (
        <svg viewBox="0 0 24 24" width="15" height="15">
          <path
            d="M8.5 4C6.8 4 6 5 6 6.7v3c0 1.1-.6 1.7-1.7 1.8v1c1.1.1 1.7.7 1.7 1.8v3c0 1.7.8 2.7 2.5 2.7h1v-1.8h-.7c-.7 0-1-.4-1-1.2v-2.8c0-1.1-.5-1.9-1.4-2.2.9-.3 1.4-1.1 1.4-2.2V7c0-.8.3-1.2 1-1.2h.7V4h-1zm7 0h-1v1.8h.7c.7 0 1 .4 1 1.2v2.8c0 1.1.5 1.9 1.4 2.2-.9.3-1.4 1.1-1.4 2.2V17c0 .8-.3 1.2-1 1.2h-.7V20h1c1.7 0 2.5-1 2.5-2.7v-3c0-1.1.6-1.7 1.7-1.8v-1c-1.1-.1-1.7-.7-1.7-1.8v-3C18 5 17.2 4 15.5 4z"
            fill="#FBBF24"
          />
        </svg>
      );

    case "toml":
      return (
        <svg viewBox="0 0 24 24" width="15" height="15">
          <path d="M4 4h4v2H6v12h2v2H4V4zm12 0h4v16h-4v-2h2V6h-2V4zm-7 3h6v2h-2v8h-2V9H9V7z" fill="#FB923C" />
        </svg>
      );

    case "yaml":
    case "yml":
      return (
        <svg viewBox="0 0 24 24" width="15" height="15">
          <rect x="4" y="5" width="16" height="2.4" rx="1" fill="#F43F5E" />
          <rect x="8" y="10.8" width="12" height="2.4" rx="1" fill="#FB7185" />
          <rect x="8" y="16.6" width="10" height="2.4" rx="1" fill="#FDA4AF" />
        </svg>
      );

    case "css":
    case "scss":
      return (
        <svg viewBox="0 0 24 24" width="15" height="15">
          <path d="M4 3l1.5 16.2L12 21l6.5-1.8L20 3H4z" fill={ext === "scss" ? "#C084FC" : "#264DE4"} />
          <path d="M12 4.6v14.8l5.1-1.4 1.2-13.4H12z" fill={ext === "scss" ? "#D8B4FE" : "#2965F1"} />
          <path d="M7.5 8h9l-.3 2.2H9.6l.2 2h6.1l-.4 4.3-3.5 1-3.5-1-.2-2.2h2l.1 1 1.6.4 1.6-.4.2-2H8l-.5-5.3z" fill="#FFFFFF" />
        </svg>
      );

    case "html":
    case "htm":
      return (
        <svg viewBox="0 0 24 24" width="15" height="15">
          <path d="M4 3l1.5 16.2L12 21l6.5-1.8L20 3H4z" fill="#E34F26" />
          <path d="M12 4.6v14.8l5.1-1.4 1.2-13.4H12z" fill="#EF652A" />
          <path d="M7.3 8h9.4l-.2 2H9.5l.2 2.1h6.6l-.5 4.4-3.8 1.1-3.8-1.1-.2-2.3h2l.1 1 1.9.5 1.9-.5.2-2.1H7.9L7.3 8z" fill="#FFFFFF" />
        </svg>
      );

    case "svg":
      return (
        <svg viewBox="0 0 24 24" width="15" height="15">
          <path d="M5 17L12 5l7 12H5z" fill="none" stroke="#FFB13B" strokeWidth="2" strokeLinejoin="round" />
          <circle cx="12" cy="5" r="2.2" fill="#FFB13B" />
          <circle cx="5" cy="17" r="2.2" fill="#FFB13B" />
          <circle cx="19" cy="17" r="2.2" fill="#FFB13B" />
        </svg>
      );

    case "md":
    case "mdx":
      return (
        <svg viewBox="0 0 24 24" width="15" height="15">
          <rect x="2" y="4.5" width="20" height="15" rx="2.5" fill="none" stroke="#519ABA" strokeWidth="1.8" />
          <path d="M5.5 15.5v-7h2l2 2.6 2-2.6h2v7h-2v-4l-2 2.5-2-2.5v4h-2zm11.2 0l-2.6-3h1.7V8.5h1.8v4h1.7l-2.6 3z" fill="#519ABA" />
        </svg>
      );

    case "sh":
    case "bash":
    case "zsh":
    case "ps1":
    case "bat":
    case "cmd":
      return (
        <svg viewBox="0 0 24 24" width="15" height="15">
          <rect x="2.5" y="4" width="19" height="16" rx="2.6" fill="rgba(74,222,128,0.14)" stroke="#4ADE80" strokeWidth="1.6" />
          <path d="M6.5 9.2l3.8 2.8-3.8 2.8M11.8 15h5.2" fill="none" stroke="#4ADE80" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" />
        </svg>
      );

    case "sql":
      return (
        <svg viewBox="0 0 24 24" width="15" height="15">
          <ellipse cx="12" cy="6.5" rx="7" ry="2.8" fill="none" stroke="#60A5FA" strokeWidth="1.8" />
          <path d="M5 6.5v11c0 1.55 3.13 2.8 7 2.8s7-1.25 7-2.8v-11" fill="none" stroke="#60A5FA" strokeWidth="1.8" />
          <path d="M5 12c0 1.55 3.13 2.8 7 2.8s7-1.25 7-2.8" fill="none" stroke="#60A5FA" strokeWidth="1.6" />
        </svg>
      );

    case "rsc":
    case "cfg":
    case "conf":
    case "ini":
      return (
        <svg viewBox="0 0 24 24" width="15" height="15">
          <circle cx="12" cy="12" r="3" fill="none" stroke="#2DD4BF" strokeWidth="1.9" />
          <path
            d="M12 3v2.5M12 18.5V21M3 12h2.5M18.5 12H21M5.6 5.6l1.8 1.8M16.6 16.6l1.8 1.8M18.4 5.6l-1.8 1.8M7.4 16.6l-1.8 1.8"
            stroke="#2DD4BF"
            strokeWidth="1.9"
            strokeLinecap="round"
          />
        </svg>
      );

    default:
      return (
        <svg viewBox="0 0 24 24" width="15" height="15">
          <path
            d="M6 3h8l4 4v14H6V3z"
            fill="none"
            stroke="#94A3B8"
            strokeWidth="1.8"
            strokeLinejoin="round"
          />
          <path d="M14 3v4h4" fill="none" stroke="#94A3B8" strokeWidth="1.8" strokeLinejoin="round" />
        </svg>
      );
  }
}
