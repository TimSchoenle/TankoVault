/**
 * Inkstone token config (design §17.1). The shipped `assets/main.css` is a self-contained
 * hand-authored stylesheet so the SPA renders without a build step; this config mirrors the
 * same tokens for anyone who prefers to build utility classes with the Tailwind CLI:
 *
 *   npx tailwindcss -i ./input.css -o ./assets/main.css --watch
 *
 * The `content` globs scan the Rust sources so class names used in `rsx!` are preserved.
 */
module.exports = {
  content: ["./src/**/*.rs", "./index.html"],
  darkMode: ["selector", '[data-theme="dark"]'],
  theme: {
    extend: {
      colors: {
        ink: {
          900: "#0E1116",
          800: "#161B22",
          700: "#232A33",
        },
        paper: "#EDE6D6",
        sumi: "#F2EFE9",
        vermilion: "#E4572E",
        jade: "#2E8B78",
        muted: "#8A94A0",
      },
      fontFamily: {
        display: ["Clash Display", "Zodiak", "ui-sans-serif", "system-ui"],
        body: ["Inter", "IBM Plex Sans", "ui-sans-serif", "system-ui"],
        mono: ["IBM Plex Mono", "ui-monospace", "monospace"],
      },
      fontSize: {
        xs: "12px",
        sm: "14px",
        base: "16px",
        lg: "20px",
        xl: "28px",
        "2xl": "40px",
      },
      borderRadius: {
        card: "12px",
      },
    },
  },
  plugins: [],
};
