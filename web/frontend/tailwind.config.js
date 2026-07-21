/**
 * TankoVault (Inkstone → TankoVault) token config — DESIGN_SPEC §2–5.
 *
 * The Tailwind CLI compiles `input.css` -> `assets/main.css` (committed). The bespoke
 * `ik-*`/component classes live as plain CSS in `input.css` (not purged); these theme
 * tokens back the utility classes used for one-off layout in `rsx!`.
 *
 *   npm run css:build   # one-shot, minified
 *   npm run css:watch   # alongside `dx serve`
 */
module.exports = {
  content: ["./src/**/*.rs", "./index.html"],
  darkMode: ["selector", '[data-theme="light"]'],
  // Note: the bespoke `ik-*`/component classes are authored as plain CSS in `input.css`
  // (below the @tailwind directives), so they are emitted verbatim and never purged — no
  // safelist needed. Add utility-class patterns here only if you compose Tailwind utility
  // names dynamically at runtime (e.g. `bg-${color}`).
  theme: {
    extend: {
      colors: {
        bg: "#0B0E13",
        rail: "#0C1016",
        surface: "#12171E",
        surface2: "#0E131A",
        surfaceFeed: "#0F141A",
        surfaceUnread: "#101720",
        border: "#1A212A",
        borderCtl: "#232A33",
        borderRow: "#161C24",
        borderSoft: "#1E262F",
        text: "#F2EFE9",
        text2: "#C9CFD6",
        text3: "#B8C0CA",
        muted: "#8A94A0",
        faint: "#5C6672",
        faint2: "#4D5763",
        iconOff: "#7C8794",
        acc: "#E4572E",
        acc2: "#F07A56",
        acc3: "#F2A993",
        accDk: "#B83A17",
        jade: "#2E8B78",
        jadeBright: "#3DA88F",
        star: "#CBA43C",
        // Content type
        "type-manga": "#6FA8DC",
        "type-manhwa": "#F07A56",
        "type-manhua": "#3DA88F",
        "type-webtoon": "#CBA43C",
        // Series status
        "status-ongoing": "#3DA88F",
        "status-completed": "#6FA8DC",
        "status-hiatus": "#CBA43C",
        "status-cancelled": "#8A94A0",
        // Provider state
        "state-active": "#3DA88F",
        "state-degraded": "#CBA43C",
        "state-challenged": "#DB4A2B",
        "state-solving": "#6FA8DC",
        "state-blocked": "#C0392B",
        "state-disabled": "#8A94A0",
        // Run state
        "run-running": "#6FA8DC",
        "run-completed": "#3DA88F",
        "run-failed": "#DB4A2B",
        "run-queued": "#8A94A0",
      },
      fontFamily: {
        display: ["Bricolage Grotesque", "ui-sans-serif", "system-ui", "sans-serif"],
        body: ["IBM Plex Sans", "ui-sans-serif", "system-ui", "sans-serif"],
        mono: ["IBM Plex Mono", "ui-monospace", "SFMono-Regular", "monospace"],
      },
      borderRadius: {
        pill: "20px",
        card: "14px",
        ctl: "10px",
        chip: "8px",
      },
      boxShadow: {
        cover: "0 8px 22px rgba(0,0,0,.35)",
        hero: "0 20px 50px rgba(0,0,0,.55)",
      },
      keyframes: {
        fade: {
          "0%": { opacity: "0", transform: "translateY(8px)" },
          "100%": { opacity: "1", transform: "translateY(0)" },
        },
        bar: { "0%": { transform: "scaleX(0)" }, "100%": { transform: "scaleX(1)" } },
        pulse: { "0%,100%": { opacity: "1" }, "50%": { opacity: ".3" } },
        flow: { "0%": { backgroundPosition: "0 0" }, "100%": { backgroundPosition: "40px 0" } },
      },
      animation: {
        fade: "fade .35s ease both",
        bar: "bar .25s ease both",
        pulse: "pulse 1.6s ease-in-out infinite",
        flow: "flow 2s linear infinite",
      },
    },
  },
  plugins: [],
};
