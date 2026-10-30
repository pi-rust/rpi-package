/** Recognise only the browser's upload-list convention, never arbitrary paths. */
export type UserMessagePart =
  | { kind: "text"; text: string }
  | { kind: "attachment"; path: string; name: string };

const UUID_PREFIX =
  /^[\da-f]{8}-[\da-f]{4}-[\da-f]{4}-[\da-f]{4}-[\da-f]{12}-/i;
const HEADER = /^(?:Attached files|已附加文件)[:：][ \t]*$/;
const FILE_EXTENSION = /\.[a-z\d]{1,12}$/i;
const INLINE_COMMENT =
  /^(.+\.(?:docx?|pdf|txt|md|xlsx?|csv|pptx?|json|ya?ml|zip|png|jpe?g|webp))[ \t]{2,}(\S.*)$/i;

export function attachmentName(path: string): string {
  const basename = path.split(/[\\/]/).pop() ?? path;
  return basename.replace(UUID_PREFIX, "") || basename;
}

function uploadLine(line: string) {
  const match = /^- (\.rpi[\\/]uploads[\\/][^\\/\u0000-\u001f]+)$/.exec(line);
  if (!match) return null;
  let path = match[1].trimEnd();
  let comment = "";
  // A complete filename wins over a possible inline comment. The UI also sees
  // manually written "file.docx  please inspect" messages; keep that prose out
  // of the filename, without changing the native message or prompt at all.
  if (!FILE_EXTENSION.test(path)) {
    const inline = INLINE_COMMENT.exec(path);
    if (inline) {
      path = inline[1];
      comment = inline[2];
    }
  }
  const basename = path.split(/[\\/]/).pop();
  if (!basename || basename === "." || basename === "..") return null;
  return {
    file: { kind: "attachment" as const, path, name: attachmentName(path) },
    comment,
  };
}

export function userMessageParts(text: string): UserMessagePart[] {
  const lines = text.split(/\r?\n/);
  const parts: UserMessagePart[] = [];
  let plain: string[] = [];
  let fence = "";
  let fenceWidth = 0;
  const flush = () => {
    const body = plain.join("\n").replace(/^\n+|\n+$/g, "");
    if (body) parts.push({ kind: "text", text: body });
    plain = [];
  };
  for (let i = 0; i < lines.length; i++) {
    const marker = /^ {0,3}(`{3,}|~{3,})(.*)$/.exec(lines[i]);
    if (marker) {
      if (!fence) {
        fence = marker[1][0];
        fenceWidth = marker[1].length;
      } else if (
        marker[1][0] === fence &&
        marker[1].length >= fenceWidth &&
        !marker[2].trim()
      )
        fence = "";
      plain.push(lines[i]);
      continue;
    }
    if (!fence && HEADER.test(lines[i]) && (i === 0 || !lines[i - 1].trim())) {
      const uploads: NonNullable<ReturnType<typeof uploadLine>>[] = [];
      let next = i + 1;
      while (next < lines.length && lines[next].startsWith("- ")) {
        const upload = uploadLine(lines[next]);
        if (!upload) break;
        uploads.push(upload);
        next++;
      }
      // A mixed list might be explanatory prose, not an attachment envelope.
      if (
        uploads.length &&
        !(next < lines.length && lines[next].startsWith("- "))
      ) {
        flush();
        for (const upload of uploads) {
          parts.push(upload.file);
          if (upload.comment)
            parts.push({ kind: "text", text: upload.comment });
        }
        i = next - 1;
        continue;
      }
    }
    plain.push(lines[i]);
  }
  if (!parts.some((part) => part.kind === "attachment"))
    return [{ kind: "text", text }];
  flush();
  return parts;
}
