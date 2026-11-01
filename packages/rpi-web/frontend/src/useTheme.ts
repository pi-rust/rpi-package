import { useEffect, useLayoutEffect, useState } from "react";

export type ThemePreference = "system" | "light" | "dark";
export type ResolvedTheme = "light" | "dark";

function readPreference(): ThemePreference {
  try {
    const saved = localStorage.getItem("rpi-theme");
    return saved === "light" || saved === "dark" ? saved : "system";
  } catch {
    return "system";
  }
}

export function useTheme() {
  const [preference, setPreference] = useState<ThemePreference>(readPreference);
  const [systemDark, setSystemDark] = useState(
    () => window.matchMedia("(prefers-color-scheme: dark)").matches,
  );
  const theme: ResolvedTheme = preference === "system"
    ? (systemDark ? "dark" : "light") : preference;

  useEffect(() => {
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const update = () => setSystemDark(media.matches);
    update();
    media.addEventListener("change", update);
    return () => media.removeEventListener("change", update);
  }, []);

  useLayoutEffect(() => {
    document.documentElement.dataset.theme = theme;
    document.querySelector('meta[name="theme-color"]')?.setAttribute(
      "content", theme === "dark" ? "#161618" : "#f5f5f7",
    );
  }, [theme]);

  useEffect(() => {
    try {
      localStorage.setItem("rpi-theme", preference);
    } catch {
      /* Optional preference: following the system still works without storage. */
    }
  }, [preference]);

  return { theme, themePreference: preference, setThemePreference: setPreference };
}
