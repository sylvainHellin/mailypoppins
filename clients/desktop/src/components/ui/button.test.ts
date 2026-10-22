import { describe, expect, it } from "vitest";
import { buttonVariants } from "@/components/ui/button";

describe("the outline button", () => {
  // The dark theme's `dark:bg-input/30` and `dark:border-input` match with the
  // same specificity as a bare `aria-pressed:` utility and come later, so a
  // pressed toggle (Theme, Reader, Editor colours, the reader's HTML and Text)
  // looked like its siblings in the dark theme until the pressed state carried
  // its own `dark:` twin.
  it("marks a pressed toggle with the selection colours in both themes", () => {
    const classes = buttonVariants({ variant: "outline" }).split(/\s+/);
    for (const c of ["aria-pressed:bg-selection", "aria-pressed:border-framing", "aria-pressed:text-selection-foreground"]) {
      expect(classes).toContain(c);
    }
    for (const c of ["dark:aria-pressed:bg-selection", "dark:aria-pressed:border-framing"]) {
      expect(classes).toContain(c);
    }
  });
});
