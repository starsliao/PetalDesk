(function exposePasswordTemplates(root) {
  "use strict";

  const HTTP_PROTOCOLS = new Set(["http:", "https:"]);
  const USERNAME_HINT = /(?:user|email|mail|login|account|identifier|phone|mobile|member)/i;
  const PASSWORD_HINT = /(?:pass|password|pwd|credential)/i;

  const BUILT_IN_TEMPLATES = Object.freeze([
    Object.freeze({
      id: "google-accounts",
      label: "Google",
      version: 1,
      mode: "two-step",
      origins: Object.freeze(["https://accounts.google.com"]),
      usernameSelectors: Object.freeze([
        'input[name="identifier"]',
        'input[type="email"]',
        'input[autocomplete="username"]',
      ]),
      passwordSelectors: Object.freeze([
        'input[name="Passwd"]',
        'input[type="password"]',
      ]),
    }),
    Object.freeze({
      id: "microsoft-work-school",
      label: "Microsoft 工作或学校账户",
      version: 1,
      mode: "two-step",
      origins: Object.freeze(["https://login.microsoftonline.com"]),
      usernameSelectors: Object.freeze([
        'input[name="loginfmt"]',
        'input[type="email"]',
        'input[autocomplete="username"]',
      ]),
      passwordSelectors: Object.freeze([
        'input[name="passwd"]',
        'input[type="password"]',
      ]),
    }),
    Object.freeze({
      id: "microsoft-personal",
      label: "Microsoft 个人账户",
      version: 1,
      mode: "two-step",
      origins: Object.freeze(["https://login.live.com"]),
      usernameSelectors: Object.freeze([
        'input[name="loginfmt"]',
        'input[type="email"]',
        'input[autocomplete="username"]',
      ]),
      passwordSelectors: Object.freeze([
        'input[name="passwd"]',
        'input[type="password"]',
      ]),
    }),
    Object.freeze({
      id: "aliyun-account",
      label: "阿里云",
      version: 1,
      mode: "password",
      origins: Object.freeze(["https://account.aliyun.com"]),
      usernameSelectors: Object.freeze([
        'input[name="loginName"]',
        'input[name="username"]',
        'input[autocomplete="username"]',
      ]),
      passwordSelectors: Object.freeze([
        'input[name="password"]',
        'input[type="password"]',
      ]),
    }),
    Object.freeze({
      id: "tencent-cloud",
      label: "腾讯云",
      version: 1,
      mode: "password",
      origins: Object.freeze(["https://cloud.tencent.com"]),
      usernameSelectors: Object.freeze([
        'input[name="account"]',
        'input[name="username"]',
        'input[autocomplete="username"]',
      ]),
      passwordSelectors: Object.freeze([
        'input[name="password"]',
        'input[type="password"]',
      ]),
    }),
    Object.freeze({
      id: "huawei-cloud",
      label: "华为云",
      version: 1,
      mode: "password",
      origins: Object.freeze(["https://auth.huaweicloud.com"]),
      usernameSelectors: Object.freeze([
        'input[name="userAccount"]',
        'input[name="username"]',
        'input[autocomplete="username"]',
      ]),
      passwordSelectors: Object.freeze([
        'input[name="password"]',
        'input[type="password"]',
      ]),
    }),
  ]);

  function exactOrigin(value) {
    let url;
    try {
      url = new URL(String(value || ""));
    } catch (_error) {
      throw new Error("A valid HTTP or HTTPS URL is required");
    }
    if (!HTTP_PROTOCOLS.has(url.protocol) || !url.hostname) {
      throw new Error("Only HTTP and HTTPS origins are supported");
    }
    return url.origin;
  }

  // Common multi-level public suffixes: hosts below these need one more label
  // to reach the registrable domain (e.g. example.co.uk, mail.sina.com.cn).
  const MULTI_LEVEL_PUBLIC_SUFFIXES = new Set([
    "ac.cn",
    "ac.uk",
    "co.in",
    "co.jp",
    "co.kr",
    "co.nz",
    "co.uk",
    "com.au",
    "com.br",
    "com.cn",
    "com.hk",
    "com.mx",
    "com.my",
    "com.sg",
    "com.tr",
    "com.tw",
    "edu.cn",
    "firm.in",
    "gov.cn",
    "net.au",
    "net.cn",
    "or.jp",
    "org.au",
    "org.cn",
    "org.uk",
  ]);

  function isIpAddressHost(host) {
    return host.includes(":") || /^\d{1,3}(?:\.\d{1,3}){3}$/.test(host);
  }

  function registrableDomain(host) {
    const parts = host.split(".");
    const suffix = parts.slice(-2).join(".");
    if (parts.length >= 3 && MULTI_LEVEL_PUBLIC_SUFFIXES.has(suffix)) {
      return parts.slice(-3).join(".");
    }
    return suffix;
  }

  function sameSite(originA, originB) {
    if (originA === originB) return true;
    let hostA;
    let hostB;
    try {
      hostA = new URL(String(originA || "")).hostname;
      hostB = new URL(String(originB || "")).hostname;
    } catch (_error) {
      return false;
    }
    if (!hostA || !hostB || isIpAddressHost(hostA) || isIpAddressHost(hostB)) {
      return false;
    }
    return registrableDomain(hostA) === registrableDomain(hostB);
  }

  function templateForOrigin(value) {
    const origin = exactOrigin(value);
    return BUILT_IN_TEMPLATES.find((template) => template.origins.includes(origin)) || null;
  }

  function stringAttribute(input, name) {
    if (!input) return "";
    if (typeof input.getAttribute === "function") {
      return String(input.getAttribute(name) || "");
    }
    return String(input[name] || "");
  }

  function fieldDescriptor(input) {
    return [
      stringAttribute(input, "name"),
      stringAttribute(input, "id"),
      stringAttribute(input, "placeholder"),
      stringAttribute(input, "aria-label"),
    ].join(" ");
  }

  function isUsableInput(input) {
    if (!input || String(input.tagName || "").toLowerCase() !== "input") return false;
    if (input.disabled || input.readOnly) return false;
    const type = String(input.type || stringAttribute(input, "type") || "text").toLowerCase();
    if (["hidden", "button", "submit", "reset", "checkbox", "radio", "file"].includes(type)) {
      return false;
    }
    if (typeof input.getClientRects === "function") {
      const rects = input.getClientRects();
      if (rects && rects.length === 0) return false;
    }
    const ariaHidden = stringAttribute(input, "aria-hidden").toLowerCase();
    return ariaHidden !== "true";
  }

  function matchesSelector(input, selector) {
    try {
      return typeof input.matches === "function" && input.matches(selector);
    } catch (_error) {
      return false;
    }
  }

  function selectorRank(input, selectors) {
    for (let index = 0; index < selectors.length; index += 1) {
      if (matchesSelector(input, selectors[index])) {
        return selectors.length - index;
      }
    }
    return 0;
  }

  function usernameScore(input, selectors) {
    if (!isUsableInput(input)) return Number.NEGATIVE_INFINITY;
    const type = String(input.type || stringAttribute(input, "type") || "text").toLowerCase();
    if (type === "password") return Number.NEGATIVE_INFINITY;

    let score = selectorRank(input, selectors) * 100;
    const autocomplete = stringAttribute(input, "autocomplete").toLowerCase();
    if (autocomplete === "username" || autocomplete === "email") score += 80;
    if (type === "email") score += 50;
    if (USERNAME_HINT.test(fieldDescriptor(input))) score += 30;
    if (["text", "email", "tel", ""].includes(type)) score += 5;
    return score;
  }

  function passwordScore(input, selectors) {
    if (!isUsableInput(input)) return Number.NEGATIVE_INFINITY;
    const type = String(input.type || stringAttribute(input, "type") || "text").toLowerCase();
    if (type !== "password") return Number.NEGATIVE_INFINITY;

    let score = selectorRank(input, selectors) * 100;
    const autocomplete = stringAttribute(input, "autocomplete").toLowerCase();
    if (autocomplete === "current-password") score += 80;
    if (autocomplete === "new-password") score += 60;
    if (PASSWORD_HINT.test(fieldDescriptor(input))) score += 20;
    return score + 10;
  }

  function normalizeRecordedSelector(value) {
    const normalized = String(value || "").trim();
    if (
      normalized.length === 0
      || normalized.length > 256
      || !/^input(?:#[A-Za-z_][A-Za-z0-9_-]{0,127}|\[(?:autocomplete|name|id|aria-label|data-testid|type)="(?:\\.|[^"\\])*"\])$/.test(normalized)
    ) {
      throw new Error("The recorded template contains an unsafe selector");
    }
    return normalized;
  }

  function normalizeSelectorList(value, name) {
    if (!Array.isArray(value) || value.length === 0 || value.length > 20) {
      throw new Error(`${name} must contain between 1 and 20 selectors`);
    }
    return Object.freeze(value.map((selector) => {
      const normalized = String(selector || "").trim();
      if (/[{};]/.test(normalized) || /(?:javascript:|expression\s*\()/i.test(normalized)) {
        throw new Error(`${name} contains an unsafe selector`);
      }
      try {
        return normalizeRecordedSelector(normalized);
      } catch (_error) {
        throw new Error(`${name} contains an unsafe selector`);
      }
    }));
  }

  function normalizeUserTemplate(value, expectedOrigin) {
    if (value == null) return null;
    if (typeof value !== "object" || Array.isArray(value)) {
      throw new Error("A recorded template object is required");
    }
    const existingOrigin = value.origin || Array.isArray(value.origins) && value.origins[0];
    const origin = exactOrigin(existingOrigin);
    if (expectedOrigin && origin !== exactOrigin(expectedOrigin)) {
      throw new Error("The recorded template origin does not match this page");
    }
    return Object.freeze({
      id: String(value.id || "user-recorded").slice(0, 128),
      label: String(value.label || "用户录制模板").slice(0, 128),
      version: Math.max(1, Number.parseInt(value.version, 10) || 1),
      mode: value.mode === "two-step" ? "two-step" : "password",
      origins: Object.freeze([origin]),
      usernameSelectors: normalizeSelectorList(value.usernameSelectors, "usernameSelectors"),
      passwordSelectors: normalizeSelectorList(value.passwordSelectors, "passwordSelectors"),
      userRecorded: true,
    });
  }

  function attributeSelector(name, value) {
    const normalized = String(value || "").trim();
    if (!normalized || normalized.length > 128 || /[\u0000-\u001f\u007f]/.test(normalized)) {
      return null;
    }
    const escaped = normalized.replace(/\\/g, "\\\\").replace(/"/g, '\\"');
    return `input[${name}="${escaped}"]`;
  }

  function selectorUniquelyIdentifies(scope, selector, input) {
    try {
      const matches = Array.from(scope.querySelectorAll(selector));
      return matches.length === 1 && matches[0] === input;
    } catch (_error) {
      return false;
    }
  }

  function recordedSelectorForInput(scope, input, expectedKind) {
    if (!scope || typeof scope.querySelectorAll !== "function" || !isUsableInput(input)) {
      throw new Error("Choose a visible, editable input field");
    }
    const type = String(input.type || stringAttribute(input, "type") || "text").toLowerCase();
    if (expectedKind === "password" && type !== "password") {
      throw new Error("Choose the password input field");
    }
    if (expectedKind === "username" && type === "password") {
      throw new Error("Choose the username input field");
    }

    const candidates = [];
    for (const name of ["autocomplete", "name", "id", "aria-label", "data-testid"]) {
      const selector = attributeSelector(name, stringAttribute(input, name));
      if (selector) candidates.push(selector);
    }
    const typeSelector = attributeSelector("type", type);
    if (typeSelector) candidates.push(typeSelector);
    const unique = candidates.find((selector) => selectorUniquelyIdentifies(scope, selector, input));
    if (!unique) {
      throw new Error("This field has no unique, stable semantic selector");
    }
    return unique;
  }

  function rankedField(inputs, scorer, selectors) {
    let selected = null;
    let selectedScore = Number.NEGATIVE_INFINITY;
    let tied = false;
    for (const input of inputs) {
      const score = scorer(input, selectors);
      if (score > selectedScore) {
        selected = input;
        selectedScore = score;
        tied = false;
      } else if (score === selectedScore && Number.isFinite(score)) {
        tied = true;
      }
    }
    return { field: selected, score: selectedScore, tied };
  }

  function identifyLoginFields(scope, options = {}) {
    if (!scope || typeof scope.querySelectorAll !== "function") {
      throw new Error("A queryable document or form is required");
    }
    const origin = exactOrigin(options.origin || root.location && root.location.href);
    const userTemplate = normalizeUserTemplate(options.userTemplate, origin);
    const builtInTemplate = templateForOrigin(origin);
    const template = userTemplate || builtInTemplate;
    const usernameSelectors = template ? template.usernameSelectors : [];
    const passwordSelectors = template ? template.passwordSelectors : [];
    const inputs = Array.from(scope.querySelectorAll("input"));
    const username = rankedField(inputs, usernameScore, usernameSelectors);
    const passwordInputs = inputs
      .filter((input) => passwordScore(input, passwordSelectors) > Number.NEGATIVE_INFINITY)
      .sort((left, right) => passwordScore(right, passwordSelectors) - passwordScore(left, passwordSelectors));
    const password = passwordInputs[0] || null;
    const passwordTemplateMatches = template
      ? passwordInputs.filter((input) => selectorRank(input, passwordSelectors) > 0)
      : [];
    const passwordTemplateMatchUnique = passwordTemplateMatches.length === 1
      && passwordTemplateMatches[0] === password;
    const ambiguous = username.tied || passwordInputs.length > 1 && (
      passwordScore(passwordInputs[0], passwordSelectors)
        === passwordScore(passwordInputs[1], passwordSelectors)
    );
    const hasTemplateMatch = Boolean(
      template && (
        selectorRank(username.field, usernameSelectors) > 0
        || selectorRank(password, passwordSelectors) > 0
      ),
    );

    return {
      ambiguous,
      confidence: ambiguous ? "low" : hasTemplateMatch ? "high" : "medium",
      origin,
      passwordField: password,
      passwordFields: passwordInputs,
      passwordTemplateMatchUnique,
      source: hasTemplateMatch ? (userTemplate ? "user" : "built-in") : "generic",
      stage: password ? (username.field ? "single-page" : "password") : "username",
      templateId: hasTemplateMatch ? template.id : null,
      usernameField: username.field,
      usernameScore: username.score,
    };
  }

  function classifyCredential(candidate, savedEntries) {
    const origin = exactOrigin(candidate && candidate.origin);
    const username = String(candidate && candidate.username || "");
    const password = String(candidate && candidate.password || "");
    const entries = Array.isArray(savedEntries) ? savedEntries : [];
    const sameAccount = entries.find((entry) => {
      try {
        return exactOrigin(entry.origin) === origin && String(entry.username || "") === username;
      } catch (_error) {
        return false;
      }
    });
    if (!sameAccount) return { action: "new", entryId: null };
    if (String(sameAccount.password || "") === password) {
      return { action: "same", entryId: sameAccount.id || null };
    }
    return { action: "update", entryId: sameAccount.id || null };
  }

  root.PetalDeskPasswordTemplates = Object.freeze({
    BUILT_IN_TEMPLATES,
    classifyCredential,
    exactOrigin,
    identifyLoginFields,
    normalizeRecordedSelector,
    normalizeUserTemplate,
    recordedSelectorForInput,
    sameSite,
    templateForOrigin,
  });
})(typeof globalThis !== "undefined" ? globalThis : this);
