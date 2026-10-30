import {
  isValidElement,
  memo,
  useMemo,
  useState,
  type ComponentProps,
  type ReactNode,
} from "react";
import { Check, Copy } from "lucide-react";
import Markdown, { defaultUrlTransform, type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import remarkBreaks from "remark-breaks";
import rehypeHighlight from "rehype-highlight";
import { translate, type Language } from "./i18n";
import { ZoomableImage } from "./ZoomableImage";

const remarkPlugins = [remarkGfm, remarkBreaks];
// Data URLs are needed for inline images, never for navigable Markdown links.
const imageUrlTransform: ComponentProps<typeof Markdown>["urlTransform"] = (
  url,
  key,
  node,
) =>
  node.tagName === "img" &&
  key === "src" &&
  /^data:image\/[a-z0-9.+-]+(?:;[^,]*)?,/i.test(url)
    ? url
    : defaultUrlTransform(url);
const rehypePlugins: ComponentProps<typeof Markdown>["rehypePlugins"] = [
  [rehypeHighlight, { ignoreMissing: true }],
];

function codeText(value: ReactNode): string {
  if (typeof value === "string" || typeof value === "number")
    return String(value);
  if (Array.isArray(value)) return value.map(codeText).join("");
  if (isValidElement(value))
    return codeText((value.props as { children?: ReactNode }).children);
  return "";
}

function CodeBlock({
  children,
  language,
}: {
  children?: ReactNode;
  language: Language;
}) {
  const [copied, setCopied] = useState(false);
  const text = codeText(children).replace(/\n$/, "");

  async function copyCode() {
    if (!text) return;
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1600);
    } catch {
      // Clipboard access can be denied; the code remains selectable.
    }
  }

  return (
    <div className="markdown-code-block">
      <div className="markdown-code-heading">
        <span>{translate(language, "code")}</span>
        <button
          type="button"
          className="markdown-copy"
          onClick={() => void copyCode()}
          disabled={!text}
          aria-label={
            copied
              ? translate(language, "copied")
              : translate(language, "copyCode")
          }
        >
          {copied ? <Check size={12} /> : <Copy size={12} />}
          {copied
            ? translate(language, "copied")
            : translate(language, "copyCode")}
        </button>
      </div>
      <pre>{children}</pre>
    </div>
  );
}

/** Chat Markdown preserves authored line breaks without changing code whitespace. */
export const MarkdownBody = memo(function MarkdownBody({
  text,
  language,
}: {
  text: string;
  language: Language;
}) {
  const components = useMemo<Components>(
    () => ({
      img: ({ src, alt, title }) => (
        <ZoomableImage src={src} alt={alt} title={title} language={language} />
      ),
      table: ({ children }) => (
        <div
          className="markdown-table"
          tabIndex={0}
          role="region"
          aria-label={
            language === "zh"
              ? "表格，可横向滚动"
              : "Table, scroll horizontally"
          }
        >
          <table>{children}</table>
        </div>
      ),
      pre: ({ children }) => (
        <CodeBlock language={language}>{children}</CodeBlock>
      ),
    }),
    [language],
  );

  return (
    <div className="prose markdown-body">
      <Markdown
        remarkPlugins={remarkPlugins}
        rehypePlugins={rehypePlugins}
        components={components}
        urlTransform={imageUrlTransform}
        skipHtml
      >
        {text}
      </Markdown>
    </div>
  );
});
