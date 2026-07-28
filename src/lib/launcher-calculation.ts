import {
  evaluateCalculatorExpression,
  isCalculatorExpressionCandidate,
} from "./calculator";
import type { SearchResult } from "./types";

/**
 * A host-owned Spotlight result. It contains only the person's current text;
 * no expression is sent to the filesystem index or a plugin.
 */
export function launcherCalculationResults(query: string): SearchResult[] {
  const expression = query.trim();
  if (!isCalculatorExpressionCandidate(expression)) {
    return [];
  }

  const evaluation = evaluateCalculatorExpression(expression);
  if (!evaluation.valid || !evaluation.formatted) {
    return [];
  }

  return [{
    id: `builtin-calculation:${encodeURIComponent(expression)}`,
    name: `${expression} = ${evaluation.formatted}`,
    kind: "command",
    score: 1_000,
    metadata: "计算结果 · Enter 打开计算器并保留表达式",
    commandId: "ihub.tool.calculator",
    calculatorExpression: expression,
  }];
}
