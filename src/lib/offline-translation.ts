export interface OfflineTranslationPack {
  entries: Record<string, string>;
  id: string;
  name: string;
  source: string;
  target: string;
  version: number;
}

export interface OfflineTranslationResult {
  coverage: number;
  detectedSource: string;
  packId: string | null;
  packIds: string[];
  pivotLanguage: string | null;
  text: string;
  unknownSegments: string[];
}

export const DEFAULT_OFFLINE_SOURCE_LANGUAGE = "auto";
export const DEFAULT_OFFLINE_TARGET_LANGUAGE = "en";
export const MAX_OFFLINE_TRANSLATION_INPUT_CHARACTERS = 32_768;
export const MAX_OFFLINE_PACK_ENTRIES = 5_000;
export const MAX_OFFLINE_PACK_BYTES = 1024 * 1024;

const languageCode = /^[a-z]{2,3}(?:-[A-Z][a-z]{3})?(?:-[A-Z]{2}|-[0-9]{3})?$/;
const han = /\p{Script=Han}/u;
const latin = /\p{Script=Latin}/u;
const wordLike = /[\p{L}\p{N}]/u;

const builtInPairs: ReadonlyArray<readonly [string, string]> = [
  ["你好，世界", "hello, world"],
  ["你好世界", "hello world"],
  ["你好", "hello"],
  ["早上好", "good morning"],
  ["下午好", "good afternoon"],
  ["晚上好", "good evening"],
  ["晚安", "good night"],
  ["再见", "goodbye"],
  ["谢谢你", "thank you"],
  ["非常感谢", "thank you very much"],
  ["不客气", "you are welcome"],
  ["对不起", "sorry"],
  ["没关系", "it is okay"],
  ["请稍等", "please wait"],
  ["请重试", "please try again"],
  ["我爱你", "I love you"],
  ["我明白了", "I understand"],
  ["我不知道", "I do not know"],
  ["你好吗", "how are you"],
  ["我很好", "I am fine"],
  ["很高兴认识你", "nice to meet you"],
  ["你叫什么名字", "what is your name"],
  ["我的名字是", "my name is"],
  ["这是什么", "what is this"],
  ["在哪里", "where is it"],
  ["多少钱", "how much is it"],
  ["现在几点", "what time is it"],
  ["今天", "today"],
  ["明天", "tomorrow"],
  ["昨天", "yesterday"],
  ["现在", "now"],
  ["稍后", "later"],
  ["这里", "here"],
  ["那里", "there"],
  ["这个", "this"],
  ["那个", "that"],
  ["这些", "these"],
  ["那些", "those"],
  ["我们", "we"],
  ["他们", "they"],
  ["你们", "you"],
  ["我的", "my"],
  ["你的", "your"],
  ["他的", "his"],
  ["她的", "her"],
  ["一个", "a"],
  ["一些", "some"],
  ["全部", "all"],
  ["没有", "no"],
  ["不是", "is not"],
  ["可以", "can"],
  ["不能", "cannot"],
  ["需要", "need"],
  ["想要", "want"],
  ["喜欢", "like"],
  ["使用", "use"],
  ["打开", "open"],
  ["关闭", "close"],
  ["保存", "save"],
  ["删除", "delete"],
  ["复制", "copy"],
  ["粘贴", "paste"],
  ["选择", "select"],
  ["确认", "confirm"],
  ["取消", "cancel"],
  ["开始", "start"],
  ["停止", "stop"],
  ["暂停", "pause"],
  ["继续", "resume"],
  ["完成", "complete"],
  ["成功", "success"],
  ["失败", "failed"],
  ["错误", "error"],
  ["警告", "warning"],
  ["信息", "information"],
  ["设置", "settings"],
  ["语言", "language"],
  ["中文", "Chinese"],
  ["英文", "English"],
  ["翻译", "translate"],
  ["离线翻译", "offline translation"],
  ["本地离线翻译", "local offline translation"],
  ["本地", "local"],
  ["离线", "offline"],
  ["在线", "online"],
  ["网络", "network"],
  ["隐私", "privacy"],
  ["安全", "secure"],
  ["数据", "data"],
  ["文本", "text"],
  ["文件", "file"],
  ["文件夹", "folder"],
  ["路径", "path"],
  ["搜索", "search"],
  ["本地搜索", "local search"],
  ["索引", "index"],
  ["插件", "plugin"],
  ["插件中心", "plugin center"],
  ["启动器", "launcher"],
  ["颜色", "color"],
  ["取色器", "color picker"],
  ["屏幕", "screen"],
  ["录屏", "screen recording"],
  ["屏幕录制", "screen recording"],
  ["截图", "screenshot"],
  ["剪贴板", "clipboard"],
  ["历史", "history"],
  ["工具", "tool"],
  ["工作台", "workbench"],
  ["编辑器", "editor"],
  ["格式化", "format"],
  ["压缩", "minify"],
  ["验证", "validate"],
  ["查询", "query"],
  ["结果", "result"],
  ["输入", "input"],
  ["输出", "output"],
  ["应用", "application"],
  ["窗口", "window"],
  ["系统", "system"],
  ["用户", "user"],
  ["开发者", "developer"],
  ["项目", "project"],
  ["代码", "code"],
  ["版本", "version"],
  ["更新", "update"],
  ["安装", "install"],
  ["导入", "import"],
  ["导出", "export"],
  ["下载", "download"],
  ["上传", "upload"],
  ["连接", "connect"],
  ["设备", "device"],
  ["桌面", "desktop"],
  ["浏览器", "browser"],
  ["服务", "service"],
  ["模型", "model"],
  ["语言包", "language pack"],
  ["默认", "default"],
  ["支持", "support"],
  ["功能", "feature"],
  ["交互", "interaction"],
  ["界面", "interface"],
  ["快速", "fast"],
  ["简单", "simple"],
  ["重要", "important"],
  ["新的", "new"],
  ["当前", "current"],
  ["自动", "automatic"],
  ["手动", "manual"],
  ["启用", "enable"],
  ["禁用", "disable"],
  ["可用", "available"],
  ["不可用", "unavailable"],
  ["正在加载", "loading"],
  ["请", "please"],
  ["和", "and"],
  ["或", "or"],
  ["但是", "but"],
  ["因为", "because"],
  ["所以", "so"],
  ["如果", "if"],
  ["然后", "then"],
  ["也", "also"],
  ["更", "more"],
  ["很", "very"],
  ["是", "is"],
  ["有", "have"],
  ["在", "in"],
  ["从", "from"],
  ["到", "to"],
];

export const BUILTIN_ZH_EN_PACK: OfflineTranslationPack = {
  id: "builtin-zh-en-v1",
  name: "内置中英基础包",
  source: "zh-CN",
  target: "en",
  version: 1,
  entries: Object.fromEntries(builtInPairs),
};

function translationPacks(additionalPacks: readonly OfflineTranslationPack[]) {
  // The most recently imported packs are stored first. They intentionally
  // override bundled terminology while the bundled pack remains the final
  // fallback for the default Chinese-English route.
  return [...additionalPacks, BUILTIN_ZH_EN_PACK];
}

function addScore(scores: Map<string, number>, language: string, score: number) {
  scores.set(language, (scores.get(language) ?? 0) + score);
}

function languageSignals(text: string): Set<string> {
  const normalized = text.normalize("NFKC").toLocaleLowerCase().slice(0, 4_096);
  const signals = new Set(normalized.match(/[\p{L}\p{N}'-]+/gu) ?? []);
  const compactCjk = Array.from(normalized)
    .filter((character) => /[\p{Script=Han}\p{Script=Hiragana}\p{Script=Katakana}\p{Script=Hangul}]/u.test(character))
    .join("");
  const characters = Array.from(compactCjk);
  for (let start = 0; start < characters.length; start += 1) {
    for (let length = 1; length <= 8 && start + length <= characters.length; length += 1) {
      signals.add(characters.slice(start, start + length).join(""));
    }
  }
  return signals;
}

export function detectOfflineLanguage(
  text: string,
  additionalPacks: readonly OfflineTranslationPack[] = [],
): string {
  let hanCount = 0;
  let latinCount = 0;
  let kanaCount = 0;
  let hangulCount = 0;
  let cyrillicCount = 0;
  let arabicCount = 0;
  for (const character of text) {
    if (/\p{Script=Hiragana}|\p{Script=Katakana}/u.test(character)) kanaCount += 1;
    else if (/\p{Script=Hangul}/u.test(character)) hangulCount += 1;
    else if (/\p{Script=Cyrillic}/u.test(character)) cyrillicCount += 1;
    else if (/\p{Script=Arabic}/u.test(character)) arabicCount += 1;
    else if (han.test(character)) hanCount += 1;
    else if (latin.test(character)) latinCount += 1;
  }
  const packs = translationPacks(additionalPacks);
  const languages = Array.from(new Set([
    "zh-CN",
    "en",
    ...packs.flatMap((pack) => [pack.source, pack.target]),
  ]));
  const scores = new Map(languages.map((language) => [language, 0]));
  const signals = languageSignals(text);
  const normalizedInput = normalizedSource(text, "en");

  for (const language of languages) {
    const base = language.toLocaleLowerCase().split("-")[0];
    if (base === "zh") addScore(scores, language, hanCount * 8);
    if (base === "ja") addScore(scores, language, kanaCount * 40 + hanCount * 2);
    if (base === "ko") addScore(scores, language, hangulCount * 40);
    if (["ru", "uk", "bg", "sr", "mk", "be"].includes(base)) {
      addScore(scores, language, cyrillicCount * 12);
    }
    if (["ar", "fa", "ur"].includes(base)) addScore(scores, language, arabicCount * 12);
    if (base === "en") addScore(scores, language, latinCount * 2);
    else if (latinCount > 0 && ["fr", "de", "es", "it", "pt", "nl", "sv", "da", "fi"].includes(base)) {
      addScore(scores, language, latinCount);
    }
  }

  for (const pack of packs) {
    for (const [source, target] of Object.entries(pack.entries)) {
      for (const [language, phrase] of [[pack.source, source], [pack.target, target]] as const) {
        const normalized = normalizedSource(phrase, language);
        if (normalized === normalizedInput) addScore(scores, language, 1_000);
        else if (Array.from(normalized).length <= 64 && signals.has(normalized.toLocaleLowerCase())) {
          addScore(scores, language, Math.min(80, Array.from(normalized).length * 8));
        }
      }
    }
  }

  return languages.reduce((best, language) => {
    const score = scores.get(language) ?? 0;
    const bestScore = scores.get(best) ?? 0;
    return score > bestScore ? language : best;
  }, hanCount >= latinCount ? "zh-CN" : "en");
}

function normalizedSource(value: string, language: string): string {
  const normalized = value.trim().replace(/\s+/g, " ");
  return language.toLocaleLowerCase().startsWith("en")
    ? normalized.toLocaleLowerCase("en")
    : normalized;
}

function packPairs(pack: OfflineTranslationPack, source: string, target: string) {
  if (pack.source === source && pack.target === target) {
    return Object.entries(pack.entries).map(([from, to]) => [normalizedSource(from, source), to] as const);
  }
  if (pack.source === target && pack.target === source) {
    return Object.entries(pack.entries).map(([from, to]) => [normalizedSource(to, source), from] as const);
  }
  return [];
}

interface TranslationRoute {
  packIds: string[];
  pairs: ReadonlyArray<readonly [string, string]>;
}

function translationRoute(
  packs: readonly OfflineTranslationPack[],
  source: string,
  target: string,
): TranslationRoute | null {
  const entries = new Map<string, string>();
  const packIds: string[] = [];
  for (const pack of packs) {
    const pairs = packPairs(pack, source, target);
    if (pairs.length === 0) continue;
    packIds.push(pack.id);
    for (const [from, to] of pairs) {
      if (!entries.has(from)) entries.set(from, to);
    }
  }
  return entries.size > 0 ? { packIds, pairs: Array.from(entries.entries()) } : null;
}

function finishEnglish(value: string): string {
  const spaced = value
    .replace(/\s+([,.;!?%])/g, "$1")
    .replace(/([([{])\s+/g, "$1")
    .replace(/\s+([)\]}])/g, "$1")
    .replace(/\s+/g, " ")
    .trim();
  return spaced ? spaced[0].toLocaleUpperCase("en") + spaced.slice(1) : "";
}

function translateChinese(text: string, pairs: ReadonlyArray<readonly [string, string]>) {
  const entries = [...pairs].sort((left, right) => right[0].length - left[0].length);
  const punctuation: Record<string, string> = {
    "，": ",", "。": ".", "！": "!", "？": "?", "：": ":", "；": ";",
    "（": "(", "）": ")", "“": "\"", "”": "\"", "‘": "'", "’": "'",
  };
  const output: string[] = [];
  const unknown: string[] = [];
  let matchedCharacters = 0;
  let totalCharacters = 0;
  for (const character of text) if (wordLike.test(character)) totalCharacters += 1;

  for (let index = 0; index < text.length;) {
    const candidate = entries.find(([source]) => text.startsWith(source, index));
    if (candidate) {
      output.push(candidate[1]);
      matchedCharacters += Array.from(candidate[0]).filter((character) => wordLike.test(character)).length;
      index += candidate[0].length;
      continue;
    }
    const character = text[index];
    output.push(punctuation[character] ?? character);
    if (han.test(character) && !unknown.includes(character)) unknown.push(character);
    index += 1;
  }
  return {
    text: finishEnglish(output.join(" ")),
    unknown,
    coverage: totalCharacters === 0 ? 1 : matchedCharacters / totalCharacters,
  };
}

function translateSpaceDelimited(
  text: string,
  pairs: ReadonlyArray<readonly [string, string]>,
  target: string,
) {
  const tokens = text.match(/[\p{L}\p{N}'-]+|[^\s]/gu) ?? [];
  const entryMap = new Map(pairs);
  const output: string[] = [];
  const unknown: string[] = [];
  let matchedCharacters = 0;
  let totalCharacters = 0;
  for (const token of tokens) if (wordLike.test(token)) totalCharacters += Array.from(token).length;

  for (let index = 0; index < tokens.length;) {
    let consumed = 0;
    let translated: string | undefined;
    for (let length = Math.min(6, tokens.length - index); length > 0; length -= 1) {
      const phrase = tokens.slice(index, index + length).join(" ").toLocaleLowerCase("en");
      const value = entryMap.get(phrase);
      if (value !== undefined) {
        consumed = length;
        translated = value;
        matchedCharacters += tokens
          .slice(index, index + length)
          .filter((token) => wordLike.test(token))
          .reduce((sum, token) => sum + Array.from(token).length, 0);
        break;
      }
    }
    if (translated !== undefined) {
      output.push(translated);
      index += consumed;
      continue;
    }
    const token = tokens[index];
    output.push(token);
    if (wordLike.test(token) && !unknown.includes(token)) unknown.push(token);
    index += 1;
  }

  const joined = target.toLocaleLowerCase().startsWith("zh")
    ? output.join("").replace(/([，。！？：；])(?=[A-Za-z])/g, "$1 ")
    : finishEnglish(output.join(" "));
  return {
    text: joined,
    unknown,
    coverage: totalCharacters === 0 ? 1 : matchedCharacters / totalCharacters,
  };
}

export function translateOffline(
  input: string,
  sourceLanguage = DEFAULT_OFFLINE_SOURCE_LANGUAGE,
  targetLanguage = DEFAULT_OFFLINE_TARGET_LANGUAGE,
  additionalPacks: readonly OfflineTranslationPack[] = [],
): OfflineTranslationResult {
  if (input.length > MAX_OFFLINE_TRANSLATION_INPUT_CHARACTERS) {
    throw new Error(`离线翻译输入不能超过 ${MAX_OFFLINE_TRANSLATION_INPUT_CHARACTERS.toLocaleString()} 个字符。`);
  }
  const detectedSource = sourceLanguage === "auto"
    ? detectOfflineLanguage(input, additionalPacks)
    : sourceLanguage;
  if (detectedSource === targetLanguage) {
    return {
      coverage: 1,
      detectedSource,
      packId: null,
      packIds: [],
      pivotLanguage: null,
      text: input,
      unknownSegments: [],
    };
  }
  const packs = translationPacks(additionalPacks);

  const translateRoute = (
    value: string,
    source: string,
    target: string,
    route: TranslationRoute,
  ) => {
    const exact = new Map(route.pairs).get(normalizedSource(value, source));
    if (exact !== undefined) {
      return { coverage: 1, text: exact, unknown: [] as string[] };
    }
    return source.toLocaleLowerCase().startsWith("zh")
      ? translateChinese(value, route.pairs)
      : translateSpaceDelimited(value, route.pairs, target);
  };

  const direct = translationRoute(packs, detectedSource, targetLanguage);
  if (direct) {
    const translated = translateRoute(input, detectedSource, targetLanguage, direct);
    return {
      coverage: translated.coverage,
      detectedSource,
      packId: direct.packIds[0] ?? null,
      packIds: direct.packIds,
      pivotLanguage: null,
      text: translated.text,
      unknownSegments: translated.unknown,
    };
  }

  const pivotLanguage = "en";
  const firstRoute = detectedSource === pivotLanguage
    ? null
    : translationRoute(packs, detectedSource, pivotLanguage);
  const secondRoute = targetLanguage === pivotLanguage
    ? null
    : translationRoute(packs, pivotLanguage, targetLanguage);
  if (!firstRoute || !secondRoute) {
    throw new Error(`未安装 ${detectedSource} → ${targetLanguage} 的本地语言包或英语枢轴路径。`);
  }
  const first = translateRoute(input, detectedSource, pivotLanguage, firstRoute);
  const second = translateRoute(first.text, pivotLanguage, targetLanguage, secondRoute);
  const packIds = Array.from(new Set([...firstRoute.packIds, ...secondRoute.packIds]));
  return {
    coverage: Math.min(first.coverage, second.coverage),
    detectedSource,
    packId: packIds[0] ?? null,
    packIds,
    pivotLanguage,
    text: second.text,
    unknownSegments: Array.from(new Set([...first.unknown, ...second.unknown])),
  };
}

export function parseOfflineTranslationPack(serialized: string): OfflineTranslationPack {
  if (new TextEncoder().encode(serialized).byteLength > MAX_OFFLINE_PACK_BYTES) {
    throw new Error("离线语言包不能超过 1 MiB。");
  }
  const value: unknown = JSON.parse(serialized);
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("离线语言包必须是 JSON 对象。");
  }
  const candidate = value as Partial<OfflineTranslationPack>;
  if (
    typeof candidate.id !== "string"
    || !/^[a-z0-9][a-z0-9._-]{0,63}$/.test(candidate.id)
    || typeof candidate.name !== "string"
    || candidate.name.trim().length < 1
    || candidate.name.length > 80
    || typeof candidate.source !== "string"
    || !languageCode.test(candidate.source)
    || typeof candidate.target !== "string"
    || !languageCode.test(candidate.target)
    || candidate.source === candidate.target
    || candidate.version !== 1
    || !candidate.entries
    || typeof candidate.entries !== "object"
    || Array.isArray(candidate.entries)
  ) {
    throw new Error("离线语言包的 id、名称、语言或版本无效。");
  }
  const entries = Object.entries(candidate.entries);
  if (entries.length < 1 || entries.length > MAX_OFFLINE_PACK_ENTRIES) {
    throw new Error(`离线语言包必须包含 1-${MAX_OFFLINE_PACK_ENTRIES} 个词条。`);
  }
  const normalizedEntries: Record<string, string> = {};
  for (const [source, target] of entries) {
    if (
      source.trim().length < 1
      || source.length > 256
      || typeof target !== "string"
      || target.trim().length < 1
      || target.length > 1024
      || /[\u0000-\u0008\u000b\u000c\u000e-\u001f]/.test(source + target)
    ) {
      throw new Error("离线语言包包含空白、过长或控制字符词条。");
    }
    normalizedEntries[source.trim()] = target.trim();
  }
  return {
    id: candidate.id,
    name: candidate.name.trim(),
    source: candidate.source,
    target: candidate.target,
    version: 1,
    entries: normalizedEntries,
  };
}
