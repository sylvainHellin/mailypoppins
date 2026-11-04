import { describe, expect, it } from "vitest";
import { describeRecurrence, NO_REPEAT, patternLabel, repeatFromRule, repeatLabel, ruleOf, weekIndexOf } from "@/app/recurrence";

describe("a recurring event's rule", () => {
  it("names each pattern on its first day as the cards do", () => {
    const day = "2099-10-29";
    expect(patternLabel("daily", day)).toBe("Daily");
    expect(patternLabel("weekly", day)).toBe("Weekly on Thursday");
    expect(patternLabel("monthly-date", day)).toBe("Monthly on day 29");
    expect(patternLabel("monthly-weekday", day)).toBe("Monthly on the last Thursday");
    expect(patternLabel("yearly", day)).toBe("Yearly on 29 October");
    expect(weekIndexOf("2099-10-22")).toBe("fourth");
    expect(describeRecurrence({ frequency: "monthly", interval: 2, weekdays: ["thursday"], week_index: "last", count: 6 })).toBe(
      "Every 2 months on the last Thursday (6 times)",
    );
    expect(describeRecurrence({ frequency: "yearly", interval: 1, weekdays: ["thursday"], week_index: "fourth", month: 11 })).toBe(
      "Yearly on the fourth Thursday of November",
    );
  });

  it("builds the rule the daemon takes, and names what is wrong with it", () => {
    const day = "2099-10-14T09:00";
    expect(ruleOf(NO_REPEAT, day)).toEqual({ rule: null });
    const weekly = { ...NO_REPEAT, pattern: "weekly" as const, every: "2", on: ["friday", "monday"], ends: "until" as const, until: "2099-12-31" };
    expect(ruleOf(weekly, day)).toEqual({ rule: { frequency: "weekly", interval: 2, weekdays: ["monday", "friday"], until: "2099-12-31" } });
    expect(repeatLabel(weekly, day)).toBe("Every 2 weeks on Monday, Friday until 2099-12-31");
    expect(ruleOf({ ...NO_REPEAT, pattern: "monthly-weekday" }, day)).toEqual({ rule: { frequency: "monthly", interval: 1, weekdays: [], week_index: "second" } });
    expect(ruleOf({ ...weekly, every: "0" }, day)).toEqual({ problem: "Every is a number from 1 to 99" });
    expect(ruleOf({ ...weekly, until: "2099-10-01" }, day)).toEqual({ problem: "The series ends before its first day, 2099-10-14" });
    expect(ruleOf({ ...weekly, ends: "count", count: "1000" }, day)).toEqual({ problem: "A series has 1 to 999 occurrences" });
    expect(repeatLabel({ ...NO_REPEAT, series: "Weekly on Tuesday" }, day)).toBe("Weekly on Tuesday (this occurrence only)");
  });

  it("reads a series' rule back into the form, or keeps one no pattern names", () => {
    const day = "2099-10-14";
    expect(repeatFromRule({ frequency: "monthly", interval: 1, weekdays: ["wednesday"], week_index: "second" }, day)).toMatchObject({
      pattern: "monthly-weekday",
      kept: null,
    });
    expect(repeatFromRule({ frequency: "monthly", interval: 1, weekdays: [], day_of_month: 3 }, day)).toMatchObject({ pattern: null });
    expect(repeatFromRule({ frequency: "daily", interval: 3, weekdays: [], until: "2099-11-01" }, day)).toMatchObject({
      pattern: "daily",
      every: "3",
      ends: "until",
      until: "2099-11-01",
    });
  });
});
