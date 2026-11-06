// Reads a field value as JSON text for display only. Tokens keep their original text, so integers of
// any size stay exact and nothing passes through JSON.parse.
export type JsonToken = {
  kind: "key" | "string" | "number" | "literal" | "punct" | "bracket";
  text: string;
  level?: number;
};
export type JsonLine = { depth: number; tokens: JsonToken[]; width: number };

// Larger values stay plain text so opening them remains quick.
const MAX_JSON_TEXT = 2_000_000;
const NUMBER = /-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?/y;
const WIDE =
  /[\u1100-\u115f\u2e80-\ua4cf\uac00-\ud7a3\uf900-\ufaff\ufe30-\ufe4f\uff00-\uff60\uffe0-\uffe6]/g;

function scanString(text: string, at: number): number {
  for (let i = at + 1; i < text.length; i++) {
    const code = text.charCodeAt(i);
    if (code === 0x22) return i + 1;
    if (code < 0x20) return -1;
    if (code === 0x5c) {
      const next = text[++i];
      if (next === "u") {
        if (!/^[0-9a-fA-F]{4}$/.test(text.slice(i + 1, i + 5))) return -1;
        i += 4;
      } else if (!next || !'"\\/bfnrt'.includes(next)) return -1;
    }
  }
  return -1;
}

type Expect =
  | "value"
  | "key"
  | "keyOrClose"
  | "valueOrClose"
  | "colon"
  | "commaOrClose"
  | "end";

// Returns the value laid out as indented lines, or null unless the whole text is one JSON object or array.
export function formatJson(text: string): JsonLine[] | null {
  if (text.length > MAX_JSON_TEXT) return null;
  const lines: JsonLine[] = [];
  const stack: string[] = [];
  let current: JsonLine = { depth: 0, tokens: [], width: 0 };
  let expect: Expect = "value";
  const push = (token: JsonToken) => {
    current.tokens.push(token);
  };
  const flush = () => {
    lines.push(current);
    current = { depth: stack.length, tokens: [], width: 0 };
  };
  // After a complete value: more of the open container, or the end of the text.
  const after = (): Expect => (stack.length ? "commaOrClose" : "end");
  let i = 0;
  while (i < text.length) {
    const char = text[i];
    if (char === " " || char === "\t" || char === "\n" || char === "\r") {
      i++;
      continue;
    }
    if (expect === "end") return null;
    if (char === "{" || char === "[") {
      if (expect !== "value" && expect !== "valueOrClose") return null;
      push({ kind: "bracket", text: char, level: stack.length % 3 });
      const close = char === "{" ? "}" : "]";
      let next = i + 1;
      while (next < text.length && " \t\n\r".includes(text[next])) next++;
      // Empty containers stay on one line.
      if (text[next] === close) {
        push({ kind: "bracket", text: close, level: stack.length % 3 });
        i = next + 1;
        expect = after();
        continue;
      }
      stack.push(char);
      expect = char === "{" ? "keyOrClose" : "valueOrClose";
      flush();
      i++;
      continue;
    }
    if (char === "}" || char === "]") {
      const open = char === "}" ? "{" : "[";
      if (stack[stack.length - 1] !== open || expect !== "commaOrClose")
        return null;
      stack.pop();
      flush();
      push({ kind: "bracket", text: char, level: stack.length % 3 });
      i++;
      expect = after();
      continue;
    }
    if (char === ",") {
      if (expect !== "commaOrClose") return null;
      push({ kind: "punct", text: "," });
      flush();
      expect = stack[stack.length - 1] === "{" ? "key" : "value";
      i++;
      continue;
    }
    if (char === ":") {
      if (expect !== "colon") return null;
      push({ kind: "punct", text: ": " });
      expect = "value";
      i++;
      continue;
    }
    if (char === '"') {
      const end = scanString(text, i);
      if (end < 0) return null;
      if (!stack.length) return null;
      const key = expect === "key" || expect === "keyOrClose";
      if (!key && expect !== "value" && expect !== "valueOrClose") return null;
      push({ kind: key ? "key" : "string", text: text.slice(i, end) });
      i = end;
      if (key) expect = "colon";
      else expect = after();
      continue;
    }
    if (expect !== "value" && expect !== "valueOrClose") return null;
    // A bare scalar at the top is ordinary text, not a JSON document worth a separate view.
    if (!stack.length) return null;
    const literal = ["true", "false", "null"].find((word) =>
      text.startsWith(word, i),
    );
    if (literal) {
      push({ kind: "literal", text: literal });
      i += literal.length;
      expect = after();
      continue;
    }
    NUMBER.lastIndex = i;
    const number = NUMBER.exec(text);
    if (!number) return null;
    push({ kind: "number", text: number[0] });
    i += number[0].length;
    expect = after();
  }
  if (expect !== "end") return null;
  if (current.tokens.length) lines.push(current);
  for (const line of lines) {
    let width = line.depth * 2;
    for (const token of line.tokens)
      width += token.text.length + (token.text.match(WIDE)?.length || 0);
    line.width = width;
  }
  return lines;
}

export function jsonLinesText(lines: JsonLine[]): string {
  return lines
    .map(
      (line) =>
        "  ".repeat(line.depth) +
        line.tokens.map((token) => token.text).join(""),
    )
    .join("\n");
}
