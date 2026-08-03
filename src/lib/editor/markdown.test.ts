import { describe, expect, it } from "vitest";
import { extractLocalImagePaths, renderMarkdown } from "./markdown";

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

  it("renders HTTP and HTTPS Markdown images without allowing unsafe protocols", () => {
    const html = renderMarkdown([
      "![HTTP 图片](http://example.com/image.png)",
      "![安全图片](https://example.com/image.png)",
      "![危险图片](javascript:alert(1))",
      "![本地文件](file:///C:/secret.png)",
      "![协议相对地址](//example.com/image.png)",
    ].join("\n\n"));
    const document = new DOMParser().parseFromString(html, "text/html");
    const images = document.querySelectorAll<HTMLImageElement>("img");

    expect(images).toHaveLength(2);
    expect(images[0]?.getAttribute("src")).toBe("http://example.com/image.png");
    expect(images[1]?.getAttribute("src")).toBe("https://example.com/image.png");
    expect(Array.from(images).every((image) => image.getAttribute("referrerpolicy") === "no-referrer")).toBe(true);
    expect(html).not.toContain('src="javascript:');
    expect(html).not.toContain('src="file:');
    expect(html).not.toContain('src="//example.com');
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

  it("keeps GFM table structure and safe column alignment", () => {
    const html = renderMarkdown([
      "| 名称 | 状态 | 备注 |",
      "| :--- | :---: | ---: |",
      "| 文档 | 完成 | **已发布** |",
    ].join("\n"));
    const document = new DOMParser().parseFromString(html, "text/html");
    const table = document.querySelector("table");

    expect(table).not.toBeNull();
    expect(table?.querySelectorAll("thead th")).toHaveLength(3);
    expect(table?.querySelectorAll("tbody td")).toHaveLength(3);
    expect(table?.querySelector("tbody strong")?.textContent).toBe("已发布");
    expect(table?.querySelector("th:nth-child(1)")?.classList.contains("md-align-left")).toBe(true);
    expect(table?.querySelector("th:nth-child(2)")?.classList.contains("md-align-center")).toBe(true);
    expect(table?.querySelector("th:nth-child(3)")?.classList.contains("md-align-right")).toBe(true);
    expect(table?.querySelector("[style]")).toBeNull();
  });

  it("renders safe HTML images inside GFM table cells", () => {
    const markdown = [
      "| 任务甘特图 | 截图与长截图 |",
      "| --- | --- |",
      '| <img src="assets/gantt.png" alt="任务甘特图" width="500" /> | <img src="assets/screenshot.png" alt="截图工具" width="500" /> |',
    ].join("\n");
    const html = renderMarkdown(markdown, {
      assetUrls: {
        "assets/gantt.png": "data:image/png;base64,Z2FudHQ=",
        "assets/screenshot.png": "data:image/png;base64,c2NyZWVuc2hvdA==",
      },
    });
    const document = new DOMParser().parseFromString(html, "text/html");
    const images = document.querySelectorAll<HTMLImageElement>("tbody td img");

    expect(images).toHaveLength(2);
    expect(images[0]?.getAttribute("alt")).toBe("任务甘特图");
    expect(images[0]?.getAttribute("width")).toBe("500");
    expect(images[1]?.getAttribute("alt")).toBe("截图工具");
    expect(images[1]?.getAttribute("loading")).toBe("lazy");
  });

  it("renders safe local and remote HTML images while rejecting unsafe sources", () => {
    const html = renderMarkdown([
      '<img src="assets/example.png" alt="安全图片" width="500" onerror="alert(1)" style="position:fixed">',
      '<img src="https://example.com/tracker.png" alt="远程图片">',
      '<img src="file:///C:/secret.png" alt="本地文件">',
    ].join("\n\n"), {
      assetUrls: { "assets/example.png": "data:image/png;base64,aGVsbG8=" },
    });
    const document = new DOMParser().parseFromString(html, "text/html");
    const image = document.querySelector<HTMLImageElement>("img");

    expect(document.querySelectorAll("img")).toHaveLength(2);
    expect(image?.getAttribute("alt")).toBe("安全图片");
    expect(image?.getAttribute("width")).toBe("500");
    expect(image?.hasAttribute("onerror")).toBe(false);
    expect(image?.hasAttribute("style")).toBe(false);
    expect(document.querySelectorAll(".md-image-placeholder")).toHaveLength(1);
    expect(document.querySelector('img[src^="file:"]')).toBeNull();
    expect(document.querySelector('img[src^="https:"]')?.getAttribute("alt")).toBe("远程图片");
  });

  it("escapes unsupported raw HTML and removes invalid image dimensions", () => {
    const html = renderMarkdown([
      '<div onclick="alert(1)">不支持的 HTML</div>',
      '<img src="assets/example.png" width="100%" height="99999">',
    ].join("\n\n"), {
      assetUrls: { "assets/example.png": "data:image/png;base64,aGVsbG8=" },
    });
    const document = new DOMParser().parseFromString(html, "text/html");
    const image = document.querySelector<HTMLImageElement>("img");

    expect(document.querySelector("div[onclick]")).toBeNull();
    expect(document.body.textContent).toContain('<div onclick="alert(1)">不支持的 HTML</div>');
    expect(image?.hasAttribute("width")).toBe(false);
    expect(image?.hasAttribute("height")).toBe(false);
  });

  it("collects Markdown and HTML note assets without accepting path traversal", () => {
    const paths = extractLocalImagePaths([
      "![Markdown](assets/markdown.png)",
      '<img src="assets/table.png?cache=1" width="500">',
      '<img src="assets/nested/blocked.png">',
      '<img src="assets/../meta.json">',
      '<img src="https://example.com/remote.png">',
    ].join("\n\n"));

    expect(paths).toEqual(["assets/markdown.png", "assets/table.png"]);
  });
});
