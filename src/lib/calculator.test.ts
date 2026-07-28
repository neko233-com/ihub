import { describe, expect, it } from "vitest";
import {
  evaluateCalculatorExpression,
  isCalculatorExpressionCandidate,
} from "./calculator";
import { launcherCalculationResults } from "./launcher-calculation";

describe("offline calculator", () => {
  it("evaluates arithmetic without executing JavaScript", () => {
    expect(evaluateCalculatorExpression("(12 + 3) * 2^3")).toMatchObject({
      valid: true,
      formatted: "120",
    });
    expect(evaluateCalculatorExpression("2^3^2")).toMatchObject({
      valid: true,
      formatted: "512",
    });
    expect(evaluateCalculatorExpression("1 / 0")).toMatchObject({ valid: false });
    expect(evaluateCalculatorExpression("alert(1)")).toMatchObject({ valid: false });
  });

  it("does not mistake ordinary local-search text for a calculation", () => {
    expect(isCalculatorExpressionCandidate("2026")).toBe(false);
    expect(isCalculatorExpressionCandidate("2026-07-28")).toBe(false);
    expect(isCalculatorExpressionCandidate("report-2026")).toBe(false);
    expect(isCalculatorExpressionCandidate("D:\\Projects\\iHub")).toBe(false);
    expect(isCalculatorExpressionCandidate("2e3")).toBe(true);
    expect(isCalculatorExpressionCandidate("(12 + 3) * 2")).toBe(true);
  });

  it("turns only complete expressions into a prefilled Spotlight result", () => {
    expect(launcherCalculationResults(" 3 * (4 + 1) ")).toEqual([
      expect.objectContaining({
        name: "3 * (4 + 1) = 15",
        commandId: "ihub.tool.calculator",
        calculatorExpression: "3 * (4 + 1)",
      }),
    ]);
    expect(launcherCalculationResults("1 +")).toEqual([]);
    expect(launcherCalculationResults("2026")).toEqual([]);
    expect(launcherCalculationResults("2026-07-28")).toEqual([]);
  });
});
