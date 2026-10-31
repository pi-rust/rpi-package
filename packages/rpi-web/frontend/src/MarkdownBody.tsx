import {
  isValidElement,
  memo,
  useContext,
  useEffect,
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
import { MermaidBlock } from "./MermaidBlock";
import { ProjectImageContext, projectImageSource } from "./projectImages";
import { FilePreview } from "./FilePreview";
import {
  browserLink,
  inlineFileLink,
  projectFileLink,
  type ProjectFileLink,
} from "./messageLinks";
import type { Root, Element } from "hast";

// Preserve fenced/indented code, including fences without a language, and avoid
// interactive controls inside authored links.
function markCodeContext() {
  return (tree: Root) => {
    function visit(node: Root | Element, blocked = false) {
      const inside =
        blocked ||
        (node.type === "element" && ["pre", "a"].includes(node.tagName));
      if (node.type === "element" && node.tagName === "code" && blocked)
        node.properties["data-block-code"] = true;
      for (const child of node.children)
        if (child.type === "element") visit(child, inside);
    }
    visit(tree);
  };
}

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
  markCodeContext,
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
  const project = useContext(ProjectImageContext);
  const [preview, setPreview] = useState<{
    owner: string;
    file: ProjectFileLink;
  } | null>(null);
  const owner = project ? JSON.stringify([project.id, project.path]) : "";
  useEffect(() => setPreview(null), [owner]);
  const components = useMemo<Components>(
    () => ({
      a: ({ href, children, title }) => {
        const file = href ? projectFileLink(href, project) : null;
        if (file)
          return (
            <button
              type="button"
              className="message-file-link"
              title={
                title ??
                (language === "zh" ? "打开文件预览" : "Open file preview")
              }
              onClick={() => setPreview({ owner, file })}
            >
              {children}
            </button>
          );
        const kind = href ? browserLink(href) : null;
        if (kind)
          return (
            <a
              href={href}
              title={title}
              target={kind === "web" ? "_blank" : undefined}
              rel={kind === "web" ? "noopener noreferrer" : undefined}
            >
              {children}
            </a>
          );
        // Fragment links remain local; unsafe/unknown schemes have no navigation.
        return href?.startsWith("#") ? (
          <a href={href}>{children}</a>
        ) : (
          <span>{children}</span>
        );
      },
      code: ({ node, children, className }) => {
        const file = !node?.properties["data-block-code"]
          ? inlineFileLink(codeText(children), project)
          : null;
        return file ? (
          <button
            type="button"
            className="message-file-link"
            title={language === "zh" ? "打开文件预览" : "Open file preview"}
            onClick={() => setPreview({ owner, file })}
          >
            <code className={className}>{children}</code>
          </button>
        ) : (
          <code className={className}>{children}</code>
        );
      },
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
      pre: ({ children }) => {
        const props = isValidElement(children)
          ? (children.props as { className?: string })
          : undefined;
        return /(?:^|\s)language-mermaid(?:\s|$)/i.test(
          props?.className ?? "",
        ) ? (
          <MermaidBlock source={codeText(children)} language={language} />
        ) : (
          <CodeBlock language={language}>{children}</CodeBlock>
        );
      },
    }),
    [language, project, owner],
  );

  return (
    <div className="prose markdown-body">
      <Markdown
        remarkPlugins={remarkPlugins}
        rehypePlugins={rehypePlugins}
        components={components}
        urlTransform={(url, key, node) => {
          if (node.tagName === "a" && key === "href") {
            // Native drive paths must be classified before defaultUrlTransform,
            // which treats a drive letter as an unknown protocol.
            return projectFileLink(url, project) ||
              browserLink(url) ||
              url.startsWith("#")
              ? url
              : "";
          }
          return imageUrlTransform(
            node.tagName === "img" && key === "src"
              ? projectImageSource(url, project)
              : url,
            key,
            node,
          );
        }}
        skipHtml
      >
        {text}
      </Markdown>
      {project && preview?.owner === owner && (
        <FilePreview
          key={owner}
          project={project}
          file={preview.file}
          language={language}
          onClose={() => setPreview(null)}
        />
      )}
    </div>
  );
});
