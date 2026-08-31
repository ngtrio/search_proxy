import { describe, expect, it } from "vitest";
import { THEME_BOOTSTRAP_SCRIPT, themeBackgrounds } from "./theme";

function runBootstrap(storedTheme: string | null, prefersLight: boolean) {
  const documentElement = { dataset: {} as Record<string, string>, style: {} as Record<string, string> };
  const execute = new Function("document", "localStorage", "matchMedia", THEME_BOOTSTRAP_SCRIPT);
  execute(
    { documentElement },
    { getItem: () => storedTheme },
    () => ({ matches: prefersLight }),
  );
  return documentElement;
}

describe("theme bootstrap", () => {
  it("applies a stored light theme and its background before React starts", () => {
    const root = runBootstrap("light", false);

    expect(root.dataset.theme).toBe("light");
    expect(root.style.backgroundColor).toBe(themeBackgrounds.light);
    expect(root.style.colorScheme).toBe("light");
  });

  it("uses the system theme when no shared preference has been stored", () => {
    const root = runBootstrap(null, false);

    expect(root.dataset.theme).toBe("dark");
    expect(root.style.backgroundColor).toBe(themeBackgrounds.dark);
  });
});
