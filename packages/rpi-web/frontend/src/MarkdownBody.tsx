import { memo, useMemo, type ComponentProps } from "react";
import Markdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import remarkBreaks from "remark-breaks";
import rehypeHighlight from "rehype-highlight";
import type { Language } from "./i18n";

const remarkPlugins = [remarkGfm, remarkBreaks];
const rehypePlugins: ComponentProps<typeof Markdown>["rehypePlugins"] = [
  [rehypeHighlight, { ignoreMissing: true }],
];

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
    }),
    [language],
  );

  return (
    <div className="prose markdown-body">
      <Markdown
        remarkPlugins={remarkPlugins}
        rehypePlugins={rehypePlugins}
        components={components}
        skipHtml
      >
        {text}
      </Markdown>
    </div>
  );
});
