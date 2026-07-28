import { describe, expect, it } from "vitest";
import { renderMarkdown } from "./markdown";

describe("renderMarkdown", () => {
  it("renders the supported Markdown blocks", () => {
    const html = renderMarkdown("# 标题\n\n**粗体**、~~删除线~~\n\n- [x] 完成");

    expect(html).toContain("<h1>标题</h1>");
    expect(html).toContain("<strong>粗体</strong>");
    expect(html).toContain("<s>删除线</s>");
    expect(html).toContain('type="checkbox"');
  });

  it("renders double-equals highlighting as a safe mark element", () => {
    const html = renderMarkdown("普通 ==高亮 **粗体**== 内容");

    expect(html).toContain("<mark>高亮 <strong>粗体</strong></mark>");
  });

  it("does not treat raw mark HTML as supported highlight syntax", () => {
    const html = renderMarkdown('<mark onclick="alert(1)">伪造高亮</mark>');
    const document = new DOMParser().parseFromString(html, "text/html");

    expect(document.querySelector("mark")).toBeNull();
    expect(document.querySelector("[onclick]")).toBeNull();
    expect(document.body.textContent).toContain('<mark onclick="alert(1)">伪造高亮</mark>');
  });

  it("does not parse raw HTML or unsafe links", () => {
    const html = renderMarkdown('<script>alert(1)</script>\n\n[危险](javascript:alert(1))');

    expect(html).not.toContain("<script>");
    expect(html).not.toContain('href="javascript:');
  });

  it("blocks data and custom-protocol links", () => {
    const html = renderMarkdown("[data](data:text/html,test) [custom](notes://secret)");

    expect(html).not.toContain("href=");
  });

  it("only renders local images through an explicit safe mapping", () => {
    const placeholder = renderMarkdown("![截图](assets/example.png)");
    const mapped = renderMarkdown("![截图](assets/example.png)", {
      assetUrls: { "assets/example.png": "data:image/png;base64,aGVsbG8=" },
    });

    expect(placeholder).toContain("md-image-placeholder");
    expect(placeholder).not.toContain("<img");
    expect(mapped).toContain('<img src="data:image/png;base64,aGVsbG8="');
  });

  it("rejects file URLs supplied in an asset mapping", () => {
    const html = renderMarkdown("![截图](assets/example.png)", {
      assetUrls: { "assets/example.png": "file:///C:/secret.png" },
    });

    expect(html).toContain("md-image-placeholder");
    expect(html).not.toContain("file:");
  });

  it("keeps a loose list together and preserves its trailing empty item", () => {
    const html = renderMarkdown("### asaa\n\n- 111\n\n- 222\n\n- 333\n\n- 5555\n\n-");
    const document = new DOMParser().parseFromString(html, "text/html");
    const lists = document.querySelectorAll("ul");
    const items = lists[0]?.querySelectorAll(":scope > li");

    expect(lists).toHaveLength(1);
    expect(items).toHaveLength(5);
    expect(Array.from(items ?? [], (item) => item.textContent?.trim())).toEqual([
      "111",
      "222",
      "333",
      "5555",
      "",
    ]);
  });
});
