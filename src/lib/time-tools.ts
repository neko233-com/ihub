const maximumDateMilliseconds = 8_640_000_000_000_000n;

export type TimeInputKind =
  | "unix-seconds"
  | "unix-milliseconds"
  | "local-date"
  | "iso-date";

export type TimeParseResult =
  | {
      ok: true;
      epochMilliseconds: number;
      inputKind: TimeInputKind;
    }
  | {
      ok: false;
      error: string;
    };

export interface ZonedDateTime {
  dateTime: string;
  offset: string;
  timeZone: string;
  value: string;
}

export type TimeSnapshotResult =
  | {
      ok: true;
      epochMilliseconds: string;
      epochSeconds: string;
      iso: string;
      local: ZonedDateTime;
      utc: ZonedDateTime;
      selected: ZonedDateTime | null;
      selectedError?: string;
    }
  | {
      ok: false;
      error: string;
    };

const localDatePattern =
  /^(\d{4})-(\d{2})-(\d{2})(?:[ T](\d{2}):(\d{2})(?::(\d{2})(?:\.(\d{1,3}))?)?)?$/;
const isoDatePattern =
  /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})(?::(\d{2})(?:\.(\d{1,3}))?)?(Z|[+-]\d{2}:?\d{2})$/i;
const numericTimePattern = /^([+-]?)(\d+)(?:\.(\d+))?\s*(ms|s)?$/i;

function failure(error: string): TimeParseResult {
  return { ok: false, error };
}

function isLeapYear(year: number): boolean {
  return year % 4 === 0 && (year % 100 !== 0 || year % 400 === 0);
}

function daysInMonth(year: number, month: number): number {
  if (month === 2) {
    return isLeapYear(year) ? 29 : 28;
  }
  return [4, 6, 9, 11].includes(month) ? 30 : 31;
}

function validateDateParts(
  year: number,
  month: number,
  day: number,
  hour: number,
  minute: number,
  second: number,
): string | null {
  if (month < 1 || month > 12) {
    return "月份必须在 01–12 之间。";
  }
  if (day < 1 || day > daysInMonth(year, month)) {
    return "日期超出了该月份的有效范围。";
  }
  if (hour < 0 || hour > 23) {
    return "小时必须在 00–23 之间。";
  }
  if (minute < 0 || minute > 59 || second < 0 || second > 59) {
    return "分和秒必须在 00–59 之间。";
  }
  return null;
}

function parseNumericTimestamp(input: string): TimeParseResult | null {
  const match = numericTimePattern.exec(input);
  if (!match) {
    return null;
  }

  const [, signToken, digits, fractionToken, explicitUnit] = match;
  const fraction = (fractionToken ?? "").replace(/0+$/, "");
  let unit: "seconds" | "milliseconds";

  if (explicitUnit) {
    unit = explicitUnit.toLocaleLowerCase() === "ms" ? "milliseconds" : "seconds";
  } else if (fractionToken) {
    unit = "seconds";
  } else {
    if (digits.length <= 10) {
      unit = "seconds";
    } else if (digits.length >= 12) {
      unit = "milliseconds";
    } else {
      return failure("11 位整数无法可靠判断单位，请在末尾加 s 或 ms。");
    }
  }

  if (unit === "milliseconds" && fraction) {
    return failure("毫秒时间戳必须是整数。");
  }
  if (unit === "seconds" && fraction.length > 3) {
    return failure("秒时间戳最多支持 3 位小数（毫秒精度）。");
  }

  const magnitude = BigInt(digits);
  let milliseconds = unit === "seconds" ? magnitude * 1_000n : magnitude;
  if (fraction) {
    milliseconds += BigInt(fraction.padEnd(3, "0"));
  }
  if (signToken === "-") {
    milliseconds = -milliseconds;
  }

  if (milliseconds < -maximumDateMilliseconds || milliseconds > maximumDateMilliseconds) {
    return failure("时间戳超出了 JavaScript Date 可表示的范围。");
  }

  return {
    ok: true,
    epochMilliseconds: Number(milliseconds),
    inputKind: unit === "seconds" ? "unix-seconds" : "unix-milliseconds",
  };
}

function parseIsoDate(input: string): TimeParseResult | null {
  const match = isoDatePattern.exec(input);
  if (!match) {
    return null;
  }

  const [, yearToken, monthToken, dayToken, hourToken, minuteToken, secondToken, millisecondToken, zoneToken] = match;
  const year = Number(yearToken);
  const month = Number(monthToken);
  const day = Number(dayToken);
  const hour = Number(hourToken);
  const minute = Number(minuteToken);
  const second = Number(secondToken ?? "0");
  const validationError = validateDateParts(year, month, day, hour, minute, second);
  if (validationError) {
    return failure(validationError);
  }

  if (zoneToken.toUpperCase() !== "Z") {
    const [, offsetHourToken, offsetMinuteToken] =
      /^[-+](\d{2}):?(\d{2})$/.exec(zoneToken) ?? [];
    const offsetHour = Number(offsetHourToken);
    const offsetMinute = Number(offsetMinuteToken);
    if (offsetHour > 23 || offsetMinute > 59) {
      return failure("ISO 时区偏移无效。");
    }
  }

  const normalizedZone = zoneToken.length === 5 && zoneToken.toUpperCase() !== "Z"
    ? `${zoneToken.slice(0, 3)}:${zoneToken.slice(3)}`
    : zoneToken.toUpperCase();
  const normalized = [
    `${yearToken}-${monthToken}-${dayToken}`,
    "T",
    `${hourToken}:${minuteToken}:${secondToken ?? "00"}`,
    millisecondToken ? `.${millisecondToken.padEnd(3, "0")}` : "",
    normalizedZone,
  ].join("");
  const milliseconds = Date.parse(normalized);
  if (!Number.isFinite(milliseconds)) {
    return failure("ISO 日期超出了可表示范围。");
  }

  return {
    ok: true,
    epochMilliseconds: milliseconds,
    inputKind: "iso-date",
  };
}

function parseLocalDate(input: string): TimeParseResult | null {
  const match = localDatePattern.exec(input);
  if (!match) {
    return null;
  }

  const [, yearToken, monthToken, dayToken, hourToken, minuteToken, secondToken, millisecondToken] = match;
  const year = Number(yearToken);
  const month = Number(monthToken);
  const day = Number(dayToken);
  const hour = Number(hourToken ?? "0");
  const minute = Number(minuteToken ?? "0");
  const second = Number(secondToken ?? "0");
  const millisecond = Number((millisecondToken ?? "0").padEnd(3, "0"));
  const validationError = validateDateParts(year, month, day, hour, minute, second);
  if (validationError) {
    return failure(validationError);
  }

  const date = new Date(0);
  date.setFullYear(year, month - 1, day);
  date.setHours(hour, minute, second, millisecond);
  const milliseconds = date.getTime();
  if (
    !Number.isFinite(milliseconds)
    || date.getFullYear() !== year
    || date.getMonth() !== month - 1
    || date.getDate() !== day
    || date.getHours() !== hour
    || date.getMinutes() !== minute
    || date.getSeconds() !== second
    || date.getMilliseconds() !== millisecond
  ) {
    return failure("该本机日期不存在或超出了可表示范围。");
  }

  return {
    ok: true,
    epochMilliseconds: milliseconds,
    inputKind: "local-date",
  };
}

/**
 * Parses timestamp integers/fractions and strict date text without silently
 * normalizing invalid calendar dates. Date text without an offset uses the
 * machine's local timezone; ISO input must include Z or an explicit offset.
 */
export function parseTimeInput(rawInput: string): TimeParseResult {
  const input = rawInput.trim();
  if (!input) {
    return failure("请输入 Unix 时间戳或日期文本。");
  }
  if (input.length > 128) {
    return failure("时间输入过长。");
  }

  return parseNumericTimestamp(input)
    ?? parseIsoDate(input)
    ?? parseLocalDate(input)
    ?? failure("无法识别。请使用 10 位秒、13 位毫秒、YYYY-MM-DD HH:mm:ss 或带 Z/偏移的 ISO 8601。");
}

/**
 * Keeps automatic launcher discovery intentional: ordinary short numbers are
 * calculator input, while canonical 10/13-digit timestamps, explicit units,
 * and date text can open the time tool with the original value preserved.
 */
export function parseLauncherTimeInput(rawInput: string): TimeParseResult | null {
  const parsed = parseTimeInput(rawInput);
  if (!parsed.ok) {
    return null;
  }
  if (parsed.inputKind === "local-date" || parsed.inputKind === "iso-date") {
    return parsed;
  }
  const input = rawInput.trim();
  if (/(?:ms|s)$/i.test(input)) {
    return parsed;
  }
  const integer = /^[+-]?(\d+)$/.exec(input);
  return integer && (integer[1].length === 10 || integer[1].length === 13)
    ? parsed
    : null;
}

/**
 * Matches the Time tool's normal names and keywords while keeping bare
 * numbers out unless they are an unambiguous launcher timestamp. This avoids
 * keywords such as `10位` and `13位` turning a query like `1` into time intent.
 */
export function shouldOfferLauncherTimeTool(rawInput: string, searchableText: string): boolean {
  const normalized = rawInput.trim().toLocaleLowerCase();
  if (!normalized) {
    return true;
  }
  if (parseLauncherTimeInput(rawInput)?.ok) {
    return true;
  }
  if (/^[+-]?(?:\d+(?:\.\d*)?|\.\d+)$/.test(normalized)) {
    return false;
  }
  return searchableText.toLocaleLowerCase().includes(normalized);
}

export function isValidIanaTimeZone(timeZone: string): boolean {
  const normalized = timeZone.trim();
  if (!normalized) {
    return false;
  }
  try {
    new Intl.DateTimeFormat("en-US", { timeZone: normalized }).format(0);
    return true;
  } catch {
    return false;
  }
}

function partsRecord(parts: Intl.DateTimeFormatPart[]): Record<string, string> {
  return Object.fromEntries(
    parts
      .filter((part) => part.type !== "literal")
      .map((part) => [part.type, part.value]),
  );
}

/**
 * Formats a millisecond instant in an explicit IANA timezone. The fixed
 * Gregorian/Latin-numeral formatter keeps the returned value stable across UI
 * locales while still delegating timezone and DST rules to Intl.
 */
export function formatDateTimeInTimeZone(
  epochMilliseconds: number,
  timeZone: string,
): ZonedDateTime {
  if (!Number.isInteger(epochMilliseconds) || !Number.isFinite(epochMilliseconds)) {
    throw new RangeError("时间必须是有限的整数毫秒。");
  }
  const date = new Date(epochMilliseconds);
  if (Number.isNaN(date.getTime())) {
    throw new RangeError("时间超出了 JavaScript Date 可表示的范围。");
  }
  const normalizedTimeZone = timeZone.trim();
  if (!isValidIanaTimeZone(normalizedTimeZone)) {
    throw new RangeError(`未知 IANA 时区：${timeZone || "（空）"}`);
  }

  const formatter = new Intl.DateTimeFormat("en-US-u-ca-gregory-nu-latn", {
    timeZone: normalizedTimeZone,
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hourCycle: "h23",
    timeZoneName: "longOffset",
  });
  const parts = partsRecord(formatter.formatToParts(date));
  const dateTime = `${parts.year}-${parts.month}-${parts.day} ${parts.hour}:${parts.minute}:${parts.second}.${date.getUTCMilliseconds().toString().padStart(3, "0")}`;
  const offset = parts.timeZoneName === "GMT" || parts.timeZoneName === "GMT+00:00"
    ? "UTC"
    : parts.timeZoneName;
  return {
    dateTime,
    offset,
    timeZone: normalizedTimeZone,
    value: `${dateTime} ${offset}`,
  };
}

export function formatEpochSeconds(epochMilliseconds: number): string {
  if (!Number.isInteger(epochMilliseconds) || !Number.isFinite(epochMilliseconds)) {
    throw new RangeError("时间必须是有限的整数毫秒。");
  }
  const negative = epochMilliseconds < 0;
  const absolute = Math.abs(epochMilliseconds);
  const seconds = Math.floor(absolute / 1_000);
  const remainder = absolute % 1_000;
  if (!remainder) {
    return `${negative ? "-" : ""}${seconds}`;
  }
  return `${negative ? "-" : ""}${seconds}.${remainder.toString().padStart(3, "0").replace(/0+$/, "")}`;
}

export function createTimeSnapshot(
  epochMilliseconds: number,
  options: {
    localTimeZone: string;
    selectedTimeZone: string;
  },
): TimeSnapshotResult {
  try {
    const date = new Date(epochMilliseconds);
    if (
      !Number.isInteger(epochMilliseconds)
      || !Number.isFinite(epochMilliseconds)
      || Number.isNaN(date.getTime())
    ) {
      return { ok: false, error: "时间超出了 JavaScript Date 可表示的范围。" };
    }
    let selected: ZonedDateTime | null = null;
    let selectedError: string | undefined;
    try {
      selected = formatDateTimeInTimeZone(epochMilliseconds, options.selectedTimeZone);
    } catch (error) {
      selectedError = error instanceof Error ? error.message : "无法格式化指定时区。";
    }
    return {
      ok: true,
      epochMilliseconds: epochMilliseconds.toString(),
      epochSeconds: formatEpochSeconds(epochMilliseconds),
      iso: date.toISOString(),
      local: formatDateTimeInTimeZone(epochMilliseconds, options.localTimeZone),
      utc: formatDateTimeInTimeZone(epochMilliseconds, "UTC"),
      selected,
      ...(selectedError ? { selectedError } : {}),
    };
  } catch (error) {
    return {
      ok: false,
      error: error instanceof Error ? error.message : "无法格式化该时间。",
    };
  }
}
