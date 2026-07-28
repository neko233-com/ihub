export interface CalculatorEvaluation {
  valid: boolean;
  formatted?: string;
  error?: string;
}

const maxLauncherCalculatorExpressionLength = 256;

function formatCalculatorValue(value: number) {
  if (!Number.isFinite(value)) {
    throw new Error("结果超出可表示范围。");
  }
  if (Object.is(value, -0)) {
    return "0";
  }
  if (Number.isInteger(value) && Math.abs(value) <= Number.MAX_SAFE_INTEGER) {
    return String(value);
  }
  return new Intl.NumberFormat("zh-CN", {
    maximumFractionDigits: 12,
    useGrouping: false,
  }).format(value);
}

/**
 * The launcher should only surface an instant calculation for text that is
 * clearly intended as math. A bare number may be a filename, issue number, or
 * date, so it remains an ordinary local-search query.
 */
export function isCalculatorExpressionCandidate(input: string) {
  const source = input.trim();
  if (!source || source.length > maxLauncherCalculatorExpressionLength) {
    return false;
  }
  // `2026-07-28` is overwhelmingly a date or filename rather than an intent
  // to subtract two values. Keep normal ISO date searches with local files.
  if (/^\d{4}-\d{1,2}-\d{1,2}$/.test(source)) {
    return false;
  }
  if (!/^[\d\s.+\-*/%^()eE]+$/.test(source) || !/\d/.test(source)) {
    return false;
  }
  return /[+\-*/%^()]/.test(source) || /\d[eE][+\-]?\d/.test(source);
}

/** A small parser keeps calculator input offline and never evaluates JavaScript. */
export function evaluateCalculatorExpression(input: string): CalculatorEvaluation {
  const source = input.trim();
  if (!source) {
    return { valid: true };
  }
  if (!/^[\d\s.+\-*/%^()eE]+$/.test(source)) {
    return { valid: false, error: "只支持数字、+ − × ÷ %、^、括号与科学计数法。" };
  }

  let cursor = 0;
  const skipWhitespace = () => {
    while (/\s/.test(source[cursor] ?? "")) {
      cursor += 1;
    }
  };
  const consume = (character: string) => {
    skipWhitespace();
    if (source[cursor] === character) {
      cursor += 1;
      return true;
    }
    return false;
  };
  const finite = (value: number) => {
    if (!Number.isFinite(value)) {
      throw new Error("结果超出可表示范围，或发生了除以零。");
    }
    return value;
  };
  const parsePrimary = (): number => {
    skipWhitespace();
    if (consume("(")) {
      const value = parseExpression();
      if (!consume(")")) {
        throw new Error("缺少右括号。");
      }
      return value;
    }
    const numberText = source.slice(cursor).match(/^(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?/)?.[0];
    if (!numberText) {
      throw new Error("请在运算符后输入一个数字。");
    }
    cursor += numberText.length;
    return finite(Number(numberText));
  };
  const parsePower = (): number => {
    const base = parsePrimary();
    if (!consume("^")) {
      return base;
    }
    return finite(Math.pow(base, parseUnary()));
  };
  const parseUnary = (): number => {
    if (consume("+")) {
      return parseUnary();
    }
    if (consume("-")) {
      return finite(-parseUnary());
    }
    return parsePower();
  };
  const parseTerm = (): number => {
    let value = parseUnary();
    while (true) {
      if (consume("*")) {
        value = finite(value * parseUnary());
      } else if (consume("/")) {
        const divisor = parseUnary();
        if (divisor === 0) {
          throw new Error("不能除以零。");
        }
        value = finite(value / divisor);
      } else if (consume("%")) {
        const divisor = parseUnary();
        if (divisor === 0) {
          throw new Error("不能对零取余。");
        }
        value = finite(value % divisor);
      } else {
        return value;
      }
    }
  };
  const parseExpression = (): number => {
    let value = parseTerm();
    while (true) {
      if (consume("+")) {
        value = finite(value + parseTerm());
      } else if (consume("-")) {
        value = finite(value - parseTerm());
      } else {
        return value;
      }
    }
  };

  try {
    const value = parseExpression();
    skipWhitespace();
    if (cursor !== source.length) {
      throw new Error("表达式中有无法识别的部分。");
    }
    return { valid: true, formatted: formatCalculatorValue(value) };
  } catch (error) {
    return {
      valid: false,
      error: error instanceof Error ? error.message : "无法计算该表达式。",
    };
  }
}
