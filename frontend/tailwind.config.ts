import type { Config } from "tailwindcss";

export default {
  content: ["./index.html", "./src/**/*.{ts,tsx}"],
  theme: {
    extend: {
      fontFamily: {
        sans: ["Inter", "ui-sans-serif", "system-ui", "sans-serif"],
      },
      colors: {
        obsidian: "#08090d",
        panel: "#111722",
        panel2: "#161d2a",
        line: "rgba(255,255,255,0.1)",
        ember: "#ff5a2f",
        amberline: "#ffb454",
      },
      boxShadow: {
        premium: "0 18px 55px rgba(0,0,0,0.35)",
        glow: "0 0 32px rgba(255,90,47,0.22)",
      },
    },
  },
  plugins: [],
} satisfies Config;
