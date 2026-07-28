import { describe, expect, it } from "vitest";
import {
  createTimeSnapshot,
  formatDateTimeInTimeZone,
  formatEpochSeconds,
  isValidIanaTimeZone,
  parseLauncherTimeInput,
  parseTimeInput,
  shouldOfferLauncherTimeTool,
} from "./time-tools";

describe("parseTimeInput", () => {
  it("auto-detects normal Unix seconds and milliseconds", () => {
    expect(parseTimeInput("1700000000")).toEqual({
      ok: true,
      epochMilliseconds: 1_700_000_000_000,
      inputKind: "unix-seconds",
    });
    expect(parseTimeInput("1700000000000")).toEqual({
      ok: true,
      epochMilliseconds: 1_700_000_000_000,
      inputKind: "unix-milliseconds",
    });
  });

  it("handles instants before 1970 and fractional seconds exactly", () => {
    expect(parseTimeInput("-1")).toEqual({
      ok: true,
      epochMilliseconds: -1_000,
      inputKind: "unix-seconds",
    });
    expect(parseTimeInput("-0.001s")).toEqual({
      ok: true,
      epochMilliseconds: -1,
      inputKind: "unix-seconds",
    });
    expect(formatEpochSeconds(-1)).toBe("-0.001");
  });

  it("does not inherit the 2038 limitation", () => {
    const parsed = parseTimeInput("2147483648");
    expect(parsed).toEqual({
      ok: true,
      epochMilliseconds: 2_147_483_648_000,
      inputKind: "unix-seconds",
    });
    if (parsed.ok) {
      expect(new Date(parsed.epochMilliseconds).toISOString()).toBe("2038-01-19T03:14:08.000Z");
    }
  });

  it("supports explicit units and rejects ambiguous or over-precise values", () => {
    expect(parseTimeInput("12345678901").ok).toBe(false);
    expect(parseTimeInput("00000000001")).toEqual({
      ok: false,
      error: "11 位整数无法可靠判断单位，请在末尾加 s 或 ms。",
    });
    expect(parseTimeInput("12345678901s")).toEqual({
      ok: true,
      epochMilliseconds: 12_345_678_901_000,
      inputKind: "unix-seconds",
    });
    expect(parseTimeInput("12345678901ms")).toEqual({
      ok: true,
      epochMilliseconds: 12_345_678_901,
      inputKind: "unix-milliseconds",
    });
    expect(parseTimeInput("1.0001s")).toMatchObject({ ok: false });
    expect(parseTimeInput("1.5ms")).toMatchObject({ ok: false });
  });

  it("parses offset ISO text deterministically", () => {
    expect(parseTimeInput("2024-02-29T08:30:45.125+08:00")).toEqual({
      ok: true,
      epochMilliseconds: Date.parse("2024-02-29T00:30:45.125Z"),
      inputKind: "iso-date",
    });
    expect(parseTimeInput("1969-12-31T23:59:59.999Z")).toEqual({
      ok: true,
      epochMilliseconds: -1,
      inputKind: "iso-date",
    });
  });

  it("parses local date text without assuming a test-machine timezone", () => {
    const parsed = parseTimeInput("2024-07-08 09:10:11.012");
    expect(parsed.ok).toBe(true);
    if (!parsed.ok) {
      return;
    }
    const date = new Date(parsed.epochMilliseconds);
    expect([
      date.getFullYear(),
      date.getMonth() + 1,
      date.getDate(),
      date.getHours(),
      date.getMinutes(),
      date.getSeconds(),
      date.getMilliseconds(),
    ]).toEqual([2024, 7, 8, 9, 10, 11, 12]);
    expect(parsed.inputKind).toBe("local-date");
  });

  it("rejects calendar overflow, invalid offsets, empty input, and Date overflow", () => {
    expect(parseTimeInput("2023-02-29 10:00:00")).toMatchObject({ ok: false });
    expect(parseTimeInput("2024-04-31T10:00:00Z")).toMatchObject({ ok: false });
    expect(parseTimeInput("2024-01-01T10:00:00+24:00")).toMatchObject({ ok: false });
    expect(parseTimeInput("")).toMatchObject({ ok: false });
    expect(parseTimeInput("8640000000000001ms")).toMatchObject({ ok: false });
    expect(parseTimeInput("-8640000000000001ms")).toMatchObject({ ok: false });
  });
});

describe("parseLauncherTimeInput", () => {
  it("discovers canonical timestamps and date text without hijacking calculator numbers", () => {
    expect(parseLauncherTimeInput("1700000000")?.ok).toBe(true);
    expect(parseLauncherTimeInput("1700000000000")?.ok).toBe(true);
    expect(parseLauncherTimeInput("-1700000000")?.ok).toBe(true);
    expect(parseLauncherTimeInput("1.5s")?.ok).toBe(true);
    expect(parseLauncherTimeInput("2024-07-08 09:10:11")?.ok).toBe(true);
    expect(parseLauncherTimeInput("1")).toBeNull();
    expect(parseLauncherTimeInput("123456789")).toBeNull();
  });
});

describe("shouldOfferLauncherTimeTool", () => {
  const searchable = "时间与时间戳 Unix timestamp epoch 日期 时区 timezone 10位 13位";

  it("keeps valid timestamps and explicit time-tool searches discoverable", () => {
    expect(shouldOfferLauncherTimeTool("1700000000", searchable)).toBe(true);
    expect(shouldOfferLauncherTimeTool("1700000000000", searchable)).toBe(true);
    expect(shouldOfferLauncherTimeTool("13位", searchable)).toBe(true);
    expect(shouldOfferLauncherTimeTool("timestamp", searchable)).toBe(true);
  });

  it("does not turn unrelated bare numbers into time-tool matches", () => {
    expect(shouldOfferLauncherTimeTool("1", searchable)).toBe(false);
    expect(shouldOfferLauncherTimeTool("123456789", searchable)).toBe(false);
    expect(shouldOfferLauncherTimeTool("17000000000", searchable)).toBe(false);
    expect(shouldOfferLauncherTimeTool(".5", searchable)).toBe(false);
  });
});

describe("IANA timezone formatting", () => {
  it("validates IANA zones without accepting arbitrary labels", () => {
    expect(isValidIanaTimeZone("Asia/Shanghai")).toBe(true);
    expect(isValidIanaTimeZone("UTC")).toBe(true);
    expect(isValidIanaTimeZone("Mars/Olympus_Mons")).toBe(false);
  });

  it("formats UTC, local, selected-zone and ISO fields from the same instant", () => {
    const instant = Date.parse("2024-01-02T03:04:05.006Z");
    const snapshot = createTimeSnapshot(instant, {
      localTimeZone: "Asia/Shanghai",
      selectedTimeZone: "America/New_York",
    });

    expect(snapshot).toEqual({
      ok: true,
      epochMilliseconds: "1704164645006",
      epochSeconds: "1704164645.006",
      iso: "2024-01-02T03:04:05.006Z",
      local: {
        dateTime: "2024-01-02 11:04:05.006",
        offset: "GMT+08:00",
        timeZone: "Asia/Shanghai",
        value: "2024-01-02 11:04:05.006 GMT+08:00",
      },
      utc: {
        dateTime: "2024-01-02 03:04:05.006",
        offset: "UTC",
        timeZone: "UTC",
        value: "2024-01-02 03:04:05.006 UTC",
      },
      selected: {
        dateTime: "2024-01-01 22:04:05.006",
        offset: "GMT-05:00",
        timeZone: "America/New_York",
        value: "2024-01-01 22:04:05.006 GMT-05:00",
      },
    });
  });

  it("uses Intl DST rules across the New York spring-forward boundary", () => {
    const before = formatDateTimeInTimeZone(
      Date.parse("2024-03-10T06:59:59.000Z"),
      "America/New_York",
    );
    const after = formatDateTimeInTimeZone(
      Date.parse("2024-03-10T07:00:00.000Z"),
      "America/New_York",
    );

    expect(before.value).toBe("2024-03-10 01:59:59.000 GMT-05:00");
    expect(after.value).toBe("2024-03-10 03:00:00.000 GMT-04:00");
  });

  it("keeps core conversions when only the selected zone is invalid", () => {
    expect(createTimeSnapshot(0, {
      localTimeZone: "UTC",
      selectedTimeZone: "Not/A_Zone",
    })).toEqual({
      ok: true,
      epochMilliseconds: "0",
      epochSeconds: "0",
      iso: "1970-01-01T00:00:00.000Z",
      local: {
        dateTime: "1970-01-01 00:00:00.000",
        offset: "UTC",
        timeZone: "UTC",
        value: "1970-01-01 00:00:00.000 UTC",
      },
      utc: {
        dateTime: "1970-01-01 00:00:00.000",
        offset: "UTC",
        timeZone: "UTC",
        value: "1970-01-01 00:00:00.000 UTC",
      },
      selected: null,
      selectedError: "未知 IANA 时区：Not/A_Zone",
    });
  });
});
