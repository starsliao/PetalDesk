const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const vm = require("node:vm");

const fixtures = JSON.parse(
  fs.readFileSync(path.join(__dirname, "fixtures", "password-templates.json"), "utf8"),
);

function loadTemplates() {
  const context = { URL, console };
  context.globalThis = context;
  vm.createContext(context);
  vm.runInContext(
    fs.readFileSync(path.join(__dirname, "..", "src/shared/password-templates.js"), "utf8"),
    context,
    { filename: "password-templates.js" },
  );
  return context.PetalDeskPasswordTemplates;
}

class FakeInput {
  constructor({ name = "", type = "text", autocomplete = "", value = "", id = "" } = {}) {
    this.tagName = "INPUT";
    this.name = name;
    this.type = type;
    this.autocomplete = autocomplete;
    this.value = value;
    this.id = id;
    this.disabled = false;
    this.readOnly = false;
  }

  getAttribute(name) {
    return this[name] || null;
  }

  getClientRects() {
    return [{}];
  }

  matches(selector) {
    const match = selector.match(/^input\[(name|type|autocomplete)=["']([^"']+)["']\]$/);
    if (!match) return false;
    return String(this[match[1]] || "") === match[2];
  }
}

class FakeScope {
  constructor(inputs) {
    this.inputs = inputs;
  }

  querySelectorAll(selector) {
    if (selector === "input") return this.inputs;
    return this.inputs.filter((input) => input.matches(selector));
  }
}

test("all office-cloud fixtures resolve to exact built-in origins", () => {
  const templates = loadTemplates();
  for (const fixture of fixtures) {
    const template = templates.templateForOrigin(fixture.origin);
    assert.ok(template, fixture.id);
    assert.equal(template.id, fixture.id);
    assert.equal(template.mode, fixture.mode);
    const fields = templates.identifyLoginFields(
      new FakeScope([
        new FakeInput(fixture.username),
        new FakeInput(fixture.password),
      ]),
      { origin: fixture.origin },
    );
    assert.equal(fields.templateId, fixture.id);
    assert.equal(fields.usernameField.name, fixture.username.name);
    assert.equal(fields.passwordField.name, fixture.password.name);
  }
});

test("generic fallback uses accessible field hints without widening origin matching", () => {
  const templates = loadTemplates();
  const fields = templates.identifyLoginFields(
    new FakeScope([
      new FakeInput({ id: "login-email", type: "email", autocomplete: "username" }),
      new FakeInput({ id: "account-secret", type: "password", autocomplete: "current-password" }),
    ]),
    { origin: "https://example.test" },
  );
  assert.equal(fields.source, "generic");
  assert.equal(fields.confidence, "medium");
  assert.equal(fields.usernameField.id, "login-email");
  assert.equal(fields.passwordField.id, "account-secret");
  assert.equal(templates.templateForOrigin("https://accounts.google.com.evil.test"), null);
});

test("user-recorded templates override built-ins only for their exact origin", () => {
  const templates = loadTemplates();
  const userTemplate = templates.normalizeUserTemplate({
    id: "recorded-google",
    origin: "https://accounts.google.com",
    usernameSelectors: ['input[name="my-user"]'],
    passwordSelectors: ['input[name="my-pass"]'],
  });
  const fields = templates.identifyLoginFields(
    new FakeScope([
      new FakeInput({ name: "my-user", type: "text" }),
      new FakeInput({ name: "my-pass", type: "password" }),
    ]),
    { origin: "https://accounts.google.com", userTemplate },
  );
  assert.equal(fields.source, "user");
  assert.equal(fields.templateId, "recorded-google");
  assert.throws(
    () => templates.normalizeUserTemplate({
      origin: "https://accounts.google.com",
      usernameSelectors: ["input[name=foo]; body { color: red }"] ,
      passwordSelectors: ['input[type="password"]'],
    }),
    /unsafe selector/i,
  );
  assert.throws(
    () => templates.normalizeUserTemplate("recorded-google", "https://accounts.google.com"),
    /template object/i,
  );
});

test("template recording emits one stable selector and never serializes field values", () => {
  const templates = loadTemplates();
  const username = new FakeInput({
    name: "loginName",
    type: "email",
    value: "private@example.invalid",
  });
  const scope = new FakeScope([username, new FakeInput({ type: "password" })]);
  const selector = templates.recordedSelectorForInput(scope, username, "username");
  assert.equal(selector, 'input[name="loginName"]');
  assert.equal(selector.includes("private@example.invalid"), false);
  assert.equal(templates.normalizeRecordedSelector("input#login-id"), "input#login-id");
  assert.throws(
    () => templates.normalizeRecordedSelector('input[placeholder="Email"]'),
    /unsafe selector/i,
  );
});

test("password-change forms keep a username when present and expose all password fields", () => {
  const templates = loadTemplates();
  const username = new FakeInput({ name: "account", type: "email", autocomplete: "username" });
  const current = new FakeInput({ name: "currentPassword", type: "password", autocomplete: "current-password" });
  const replacement = new FakeInput({ name: "newPassword", type: "password", autocomplete: "new-password" });
  const confirmation = new FakeInput({ name: "confirmPassword", type: "password", autocomplete: "new-password" });
  const fields = templates.identifyLoginFields(
    new FakeScope([username, current, replacement, confirmation]),
    { origin: "https://example.test" },
  );
  assert.equal(fields.usernameField, username);
  assert.equal(fields.passwordFields.length, 3);
  assert.equal(fields.passwordFields.includes(replacement), true);
  assert.equal(fields.passwordFields.includes(confirmation), true);
});

test("credential matching distinguishes same, update, and new accounts", () => {
  const templates = loadTemplates();
  const entries = [
    { id: "first", origin: "https://example.test", username: "alice", password: "old" },
  ];
  assert.deepEqual(JSON.parse(JSON.stringify(
    templates.classifyCredential({ origin: "https://example.test", username: "alice", password: "old" }, entries),
  )),
    { action: "same", entryId: "first" },
  );
  assert.deepEqual(JSON.parse(JSON.stringify(
    templates.classifyCredential({ origin: "https://example.test", username: "alice", password: "new" }, entries),
  )),
    { action: "update", entryId: "first" },
  );
  assert.deepEqual(JSON.parse(JSON.stringify(
    templates.classifyCredential({ origin: "https://example.test", username: "bob", password: "new" }, entries),
  )),
    { action: "new", entryId: null },
  );
});
