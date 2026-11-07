import { test, expect } from "@playwright/test";
import type { APIRequestContext, BrowserContext, Page } from "@playwright/test";
import { appendFile, readFile, writeFile, access } from "node:fs/promises";
import { join, resolve } from "node:path";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
// @ts-expect-error Fixture is also used directly by the Node preview launcher.
import { record, longText, header } from "./fixture.mjs";

const root = process.env.QSCOPE_E2E_DIR!;
async function grantClipboard(context: BrowserContext) {
  await context.grantPermissions(
    context.browser()?.browserType().name() === "webkit"
      ? ["clipboard-read"]
      : ["clipboard-read", "clipboard-write"],
  );
}
async function sessionId(request: APIRequestContext, name: string) {
  let id: string | undefined;
  await expect
    .poll(async () => {
      const response = await request.get("/api/v1/sessions", {
        headers: { "X-QScope-Client": "web-v1" },
      });
      expect(response.ok()).toBeTruthy();
      const catalog = await response.json();
      id = catalog.sessions.find((session: { path: string }) =>
        session.path.endsWith(name),
      )?.id;
      return id;
    })
    .toBeTruthy();
  return id!;
}
async function accountOf(request: APIRequestContext, name: string) {
  let account: string | null | undefined;
  await expect
    .poll(async () => {
      const catalog = await (
        await request.get("/api/v1/sessions", {
          headers: { "X-QScope-Client": "web-v1" },
        })
      ).json();
      account = catalog.sessions.find((session: { path: string }) =>
        session.path.endsWith(name),
      )?.account;
      return account;
    })
    .toBeTruthy();
  return account!;
}
// A synthetic producer has no QQ process. Override only its UI liveness metadata.
async function markLive(page: Page, id: string) {
  await page.route("**/api/v1/sessions**", async (route) => {
    const response = await route.fetch();
    if (route.request().method() !== "GET") {
      await route.fulfill({ response });
      return;
    }
    const body = await response.json();
    if (body.sessions) {
      for (const s of body.sessions) if (s.id === id) s.producer_alive = true;
    }
    if (body.session?.id === id) body.session.producer_alive = true;
    await route.fulfill({ response, json: body });
  });
}
const direction = (page: Page, label: string) =>
  page
    .getByRole("group", { name: "过滤方向" })
    .getByRole("button", { name: label, exact: true });
// The Windows clipboard stores line breaks as CRLF.
const clipboard = (page: Page) =>
  page
    .evaluate(() => navigator.clipboard.readText())
    .then((text) => text.replace(/\r\n/g, "\n"));
// Until the copy lands the clipboard holds older text; that is retried, not thrown.
const clipboardJson = (page: Page) =>
  clipboard(page).then((text) => {
    try {
      return JSON.parse(text);
    } catch {
      return undefined;
    }
  });

test("history, exact sequence search, direction filter and both directions of field/hex selection", async ({
  page,
  request,
}) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const id = await sessionId(request, "closed.qscap");
  await page.goto(`/?session=${id}`);
  await expect(page.getByRole("main", { name: "抓包工作区" })).toBeVisible();
  await expect(page.locator("h1, aside")).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "加载更早的记录" }),
  ).toBeVisible();
  await page
    .getByRole("searchbox", { name: "搜索命令或序号" })
    .fill("SearchTarget");
  await page
    .getByRole("option", { name: "选择包 17: Test.SearchTarget" })
    .click();
  await expect(
    page.getByRole("heading", { name: "Test.SearchTarget" }),
  ).toBeVisible();
  const field = page.getByRole("button", {
    name: "字段 1 · varint: 150",
    exact: true,
  });
  await field.click();
  await expect(page.locator(".hex-byte.highlighted")).toHaveCount(3);
  await page.getByRole("button", { name: "字节 3", exact: true }).click();
  await expect(page.locator(".field-row.selected .field-label")).toHaveText(
    '"2"',
  );
  await expect(page.locator(".hex-byte.highlighted")).toHaveCount(4);
  await page
    .getByRole("searchbox", { name: "搜索命令或序号" })
    .fill("9007199254741010");
  await expect(
    page.getByRole("listbox", { name: "数据包列表" }).getByRole("option"),
  ).toHaveCount(1);
  await expect(
    page.getByRole("listbox", { name: "数据包列表" }).getByRole("option"),
  ).toContainText("Test.SearchTarget");
  await direction(page, "↓ 收").click();
  await expect(
    page.getByRole("listbox", { name: "数据包列表" }).getByRole("option"),
  ).toHaveCount(0);
  await expect(page.getByRole("listbox", { name: "数据包列表" })).toContainText(
    "没有符合筛选的包",
  );
  await direction(page, "↑ 发").click();
  await expect(
    page.getByRole("listbox", { name: "数据包列表" }).getByRole("option"),
  ).toHaveCount(1);
  await expect(direction(page, "↑ 发")).toHaveAttribute("aria-pressed", "true");
  await page.screenshot({
    path: resolve(root, "inspector.png"),
    fullPage: true,
  });
  expect(errors).toEqual([]);
});

test("explicit TLV/JCE and incomplete PB preserve accessible raw bytes", async ({
  page,
  request,
}) => {
  const id = await sessionId(request, "closed.qscap");
  await page.goto(`/?session=${id}`);
  await page
    .getByRole("searchbox", { name: "搜索命令或序号" })
    .fill("Test.TLV");
  await page.getByRole("option", { name: "选择包 18: Test.TLV" }).click();
  await page.getByRole("tab", { name: "TLV16-BE" }).click();
  await expect(
    page.getByRole("button", { name: /字段 tag 0x0106/ }),
  ).toBeVisible();
  await page.getByRole("button", { name: "解析范围" }).click();
  await page.getByLabel("解码起始偏移").fill("4");
  await page.getByLabel("解码长度").fill("2");
  await page.getByRole("button", { name: "应用", exact: true }).click();
  await page.getByRole("tab", { name: "Protobuf" }).click();
  await page
    .getByRole("button", { name: "字段 1 · varint: 1", exact: true })
    .click();
  await expect(page.getByRole("button", { name: "解析范围" })).toContainText(
    "[4, 6)",
  );
  await expect(page.getByLabel("字节选区")).toContainText("[4, 6)");
  await page
    .getByRole("searchbox", { name: "搜索命令或序号" })
    .fill("Test.JCE");
  await page.getByRole("option", { name: "选择包 19: Test.JCE" }).click();
  await page.getByRole("tab", { name: "JCE / Tars" }).click();
  await expect(
    page
      .getByRole("button", { name: /字段 tag 1 · type 6/ })
      .locator("..")
      .locator(".json-value"),
  ).toContainText("abc");
  await page
    .getByRole("searchbox", { name: "搜索命令或序号" })
    .fill("Test.NonPB");
  await page.getByRole("option", { name: "选择包 20: Test.NonPB" }).click();
  await expect(page.getByRole("button", { name: /未解码区域/ })).toBeVisible();
  await expect(
    page.getByRole("button", { name: "字节 0", exact: true }),
  ).toHaveText("ff");
});

test("partial live tail, new-row notice, freeze and resume never duplicate or change the selected packet", async ({
  page,
  request,
}) => {
  const id = await sessionId(request, "live.qscap");
  await markLive(page, id);
  await page.goto(`/?session=${id}`);
  await expect(page.getByRole("button", { name: "跟随最新" })).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await page.getByRole("option", { name: "选择包 1: Live.Ready" }).click();
  await expect(
    page.getByRole("button", { name: "已停在此处" }),
  ).toHaveAttribute("aria-pressed", "false");
  const second = record(2, "Live.Second");
  const file = join(root, "captures/live.qscap");
  await appendFile(file, second.subarray(0, 82));
  await expect(page.locator(".list-footer")).toContainText("最后一条正在写入");
  await appendFile(file, second.subarray(82));
  await expect(
    page.getByRole("option", { name: "选择包 2: Live.Second" }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: /新增 1 条 · 回到最新/ }),
  ).toBeVisible();
  await expect(page.getByRole("heading", { name: "Live.Ready" })).toBeVisible();
  await expect(
    page.getByRole("option", { name: "选择包 1: Live.Ready" }),
  ).toHaveAttribute("aria-selected", "true");
  await page.getByRole("button", { name: "冻结", exact: true }).click();
  await appendFile(file, record(3, "Live.WhilePaused"));
  await expect(page.locator(".frozen-pill")).toContainText("新到 1 条未显示");
  await expect(
    page.getByRole("option", { name: "选择包 3: Live.WhilePaused" }),
  ).toHaveCount(0);
  await page
    .locator(".frozen-pill")
    .getByRole("button", { name: "继续刷新", exact: true })
    .click();
  await expect(
    page.getByRole("option", { name: "选择包 3: Live.WhilePaused" }),
  ).toBeVisible();
  await expect(
    page.getByRole("listbox", { name: "数据包列表" }).getByRole("option"),
  ).toHaveCount(3);
  await expect(page.getByRole("heading", { name: "Live.Ready" })).toBeVisible();
  await expect(page.getByRole("button", { name: "跟随最新" })).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await page.unrouteAll({ behavior: "wait" });
});

test("history pagination and controls keep footer and loss visibility", async ({
  page,
  request,
}) => {
  const id = await sessionId(request, "closed.qscap");
  await page.goto(`/?session=${id}`);
  await page.getByRole("button", { name: "加载更早的记录" }).click();
  await expect(page.locator(".list-footer")).toContainText("1,000 条");
  await expect(page.locator(".frozen-pill")).toHaveCount(0);
  await page.getByRole("button", { name: "高级筛选", exact: true }).click();
  await page
    .getByRole("checkbox", {
      name: "显示采集事件（启动、丢包、结束）",
      exact: true,
    })
    .check();
  await page.getByRole("button", { name: "应用筛选", exact: true }).click();
  await expect(
    page.getByRole("list", { name: "生效的筛选条件" }),
  ).toContainText("含采集事件");
  await page
    .getByRole("searchbox", { name: "搜索命令或序号" })
    .fill("controller_stopped");
  await page
    .getByRole("option", { name: "选择包 1201: controller_stopped" })
    .click();
  await expect(page.locator(".control-description")).toContainText(
    "累计丢弃 3",
  );
  await expect(page.locator(".loss-counter.bad")).toContainText("3");
});

test("imports, raw export and 1 MiB hex virtualization use the real local backend", async ({
  page,
}) => {
  await page.goto("/");
  await page
    .getByLabel("导入抓包文件")
    .setInputFiles(join(root, "upload.qscap"));
  await expect(
    page.getByRole("heading", { name: "Imported.Packet" }),
  ).toBeVisible();
  await page.getByRole("button", { name: "更多操作" }).click();
  const downloaded = page.waitForEvent("download");
  await page.getByRole("menuitem", { name: "导出原始包体" }).click();
  const download = await downloaded;
  expect(await readFile((await download.path())!)).toEqual(
    Buffer.from([8, 42]),
  );
  await page
    .getByLabel("导入抓包文件")
    .setInputFiles(join(root, "large.qscap"));
  await expect(
    page.getByRole("heading", { name: "Large.Payload" }),
  ).toBeVisible();
  await page.getByRole("tab", { name: "Hex", exact: true }).click();
  await expect(page.locator(".hex-byte").first()).toHaveText("5a");
  expect(await page.locator(".hex-byte").count()).toBeLessThan(1000);
  await page.locator(".hex-scroll").evaluate((element) => {
    element.scrollTop = element.scrollHeight;
  });
  await expect(page.locator('[data-offset="1048575"]')).toHaveText("5a");
  await page.setViewportSize({ width: 1000, height: 760 });
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBeTruthy();
});

test("compact panels keep readable text and give packet details the full available width", async ({
  page,
  request,
}) => {
  await page.setViewportSize({ width: 760, height: 900 });
  const id = await sessionId(request, "closed.qscap");
  await page.goto(`/?session=${id}`);
  await expect(
    page.getByRole("tablist", { name: "采集记录快捷切换" }),
  ).toBeVisible();
  await expect(page.locator("aside")).toHaveCount(0);
  await page
    .getByRole("searchbox", { name: "搜索命令或序号" })
    .fill("SearchTarget");
  const packet = page.getByRole("option", {
    name: "选择包 17: Test.SearchTarget",
  });
  await expect(packet).toBeVisible();
  expect(
    await packet
      .locator(".command")
      .evaluate((element) => parseFloat(getComputedStyle(element).fontSize)),
  ).toBeGreaterThanOrEqual(14);
  await packet.click();
  await expect(
    page.getByRole("heading", { name: "Test.SearchTarget" }),
  ).toBeVisible();
  await expect(page.getByRole("listbox", { name: "数据包列表" })).toBeHidden();
  await expect(
    page.getByRole("separator", { name: "调整列表与详情宽度" }),
  ).toBeHidden();
  await page.getByRole("button", { name: "返回列表", exact: true }).click();
  await expect(page.getByRole("listbox", { name: "数据包列表" })).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "Test.SearchTarget" }),
  ).toBeHidden();
  await packet.click();
  await expect(
    page.getByRole("button", { name: "字节 0", exact: true }),
  ).toHaveText("08");
  expect(
    await page
      .locator(".hex-byte")
      .first()
      .evaluate((element) => parseFloat(getComputedStyle(element).fontSize)),
  ).toBeGreaterThanOrEqual(14);
  const width = await page
    .getByRole("region", { name: "包详情" })
    .evaluate((element) => element.getBoundingClientRect().width);
  expect(width).toBeGreaterThan(700);
  await page.screenshot({
    path: resolve(root, "compact-760.png"),
    fullPage: true,
  });
  await page.getByRole("button", { name: "包列表", exact: true }).click();
  await expect(page.getByRole("listbox", { name: "数据包列表" })).toBeVisible();
  await page.setViewportSize({ width: 480, height: 900 });
  await packet.click();
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBeTruthy();
  await page.screenshot({
    path: resolve(root, "compact-480.png"),
    fullPage: true,
  });
});

test("Douyin poster appearance uses local original fonts and persists light/dark selection", async ({
  page,
  request,
}) => {
  const failures: string[] = [];
  page.on("pageerror", (error) => failures.push(error.message));
  page.on("response", (response) => {
    if (response.url().includes("/assets/") && !response.ok())
      failures.push(`${response.status()} ${response.url()}`);
  });
  const id = await sessionId(request, "closed.qscap");
  await page.goto(`/?session=${id}`);
  await expect(
    page.getByRole("button", { name: "切换深色外观" }),
  ).toBeVisible();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  const logo = await request.get("/favicon.svg");
  expect(logo.ok()).toBeTruthy();
  expect(logo.headers()["content-type"]).toContain("image/svg+xml");
  expect(await logo.text()).toBe(
    await readFile(new URL("../public/favicon.svg", import.meta.url), "utf8"),
  );
  await expect(page.locator(".brand-mark")).toHaveCSS(
    "mask-image",
    /favicon\.svg/,
  );
  await expect(page.locator(".brand-mark")).toHaveCSS(
    "background-color",
    "rgb(109, 40, 255)",
  );
  await expect(
    page.getByRole("heading", { name: /Test.Command/ }),
  ).toBeVisible();
  await page.evaluate(() => document.fonts.ready);
  expect(
    await page.evaluate(() =>
      [...document.fonts].some(
        (face) =>
          face.family.includes("HarmonyOS Sans SC") && face.status === "loaded",
      ),
    ),
  ).toBeTruthy();
  expect(
    await page.evaluate(() =>
      [...document.fonts].some(
        (face) => face.family.includes("Inter") && face.status === "loaded",
      ),
    ),
  ).toBeTruthy();
  await page.screenshot({
    path: resolve(root, "poster-light.png"),
    fullPage: true,
  });
  await page.getByRole("button", { name: "切换深色外观" }).click();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await expect(page.locator(".brand-mark")).toHaveCSS(
    "background-color",
    "rgb(173, 139, 255)",
  );
  await page.reload();
  await expect(
    page.getByRole("button", { name: "切换浅色外观" }),
  ).toBeVisible();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await page.evaluate(() => document.fonts.ready);
  await page.screenshot({
    path: resolve(root, "poster-dark.png"),
    fullPage: true,
  });
  const license = await request.get("/licenses/HarmonyOSSans-LICENSE.txt");
  expect(license.ok()).toBeTruthy();
  expect(await license.text()).toContain("Copyright 2021 Huawei");
  expect(failures).toEqual([]);
});

test("top record tabs recover workspace space and close only views, not historical evidence", async ({
  page,
  request,
}) => {
  const closed = await sessionId(request, "closed.qscap");
  const live = await sessionId(request, "live.qscap");
  const before = await readFile(join(root, "captures/closed.qscap"));
  await page.goto(`/?session=${closed}`);
  const tabs = page.getByRole("tablist", { name: "采集记录快捷切换" });
  await expect(tabs.getByRole("tab")).toHaveCount(1);
  await expect(page.locator("h1, aside, .stats-bar")).toHaveCount(0);
  await expect(page.locator(".compact-view-switch")).toBeHidden();
  await expect(page.getByRole("listbox", { name: "数据包列表" })).toBeVisible();
  const layout = await page.locator(".inspector").boundingBox();
  expect(layout!.y).toBeLessThan(130);
  expect(layout!.width).toBeGreaterThan(1520);
  expect(layout!.height).toBeGreaterThan(800);
  await page.getByRole("button", { name: /^历史记录/ }).click();
  const history = page.getByRole("dialog", { name: "历史抓包" });
  await expect(history).toContainText("每次启动对应一份原始文件");
  await expect(
    history.getByRole("region", { name: "未识别账号" }),
  ).toBeVisible();
  await history.locator(`[data-session-id="${live}"]`).click();
  await expect(tabs.getByRole("tab")).toHaveCount(2);
  await expect(page).toHaveURL(new RegExp(live));
  // Tabs are ordered newest first, like the history list.
  await expect(tabs.getByRole("tab").first()).not.toContainText("已结束");
  await tabs.getByRole("tab", { name: /已结束/ }).click();
  await expect(page).toHaveURL(new RegExp(closed));
  await page
    .getByRole("button", { name: `关闭记录视图 123 ${closed}`, exact: true })
    .click();
  await expect(tabs.getByRole("tab")).toHaveCount(1);
  await expect(page).toHaveURL(new RegExp(live));
  expect(await readFile(join(root, "captures/closed.qscap"))).toEqual(before);
  await page
    .getByRole("button", { name: `关闭记录视图 123 ${live}`, exact: true })
    .click();
  await expect(tabs.getByRole("tab")).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "打开最近的记录" }),
  ).toBeVisible();
  await page.getByRole("button", { name: /^历史记录/ }).click();
  await expect(
    history.locator(`[data-session-id="${closed}"]`),
  ).toHaveAccessibleName(/^打开QQ 启动 PID 123 /);
  await history.locator(`[data-session-id="${closed}"]`).click();
  await expect(tabs.getByRole("tab")).toHaveCount(1);
  await expect(
    page.getByRole("heading", { name: /Test.Command/ }),
  ).toBeVisible();
  await page.evaluate(() => document.fonts.ready);
  await page.screenshot({
    path: resolve(root, "top-tabs.png"),
    fullPage: true,
  });
});

test("detail expands, collapses and highlights fields, raw bytes give space back, and both popup entries work", async ({
  page,
  request,
}) => {
  const id = await sessionId(request, "closed.qscap");
  await page.goto(`/?session=${id}`);
  await page
    .getByRole("searchbox", { name: "搜索命令或序号" })
    .fill("SearchTarget");
  await page
    .getByRole("option", { name: "选择包 17: Test.SearchTarget" })
    .click();
  // Fields open fully by default; the toolbar folds and reopens them.
  const tree = page.getByRole("tree", { name: "解码字段树" });
  await expect(tree.getByRole("treeitem")).toHaveCount(7);
  await expect(tree.getByRole("treeitem", { expanded: true })).toHaveCount(1);
  await page.getByRole("button", { name: "全部折叠", exact: true }).click();
  await expect(tree.getByRole("treeitem")).toHaveCount(6);
  await expect(tree.locator(".json-count")).toHaveText("1 项");
  await page.getByRole("button", { name: "全部展开", exact: true }).click();
  await expect(tree.getByRole("treeitem")).toHaveCount(7);
  // Clicking anywhere on an object line folds it, and clicking the folded braces opens it again.
  const branch = tree.locator(".field-row.branch");
  await branch.locator(".json-key").click();
  await expect(tree.getByRole("treeitem")).toHaveCount(6);
  await expect(branch).toHaveClass(/selected/);
  await branch.locator(".json-fold").click();
  await expect(tree.getByRole("treeitem")).toHaveCount(7);
  await page.getByRole("searchbox", { name: "高亮字段" }).fill("150");
  await expect(tree.locator("mark")).toHaveText("150");
  // Raw bytes sit beside the tree; folding them gives the width back.
  const width = (await page.locator(".decode-area").boundingBox())!.width;
  await page.getByRole("button", { name: "折叠原始字节" }).click();
  await expect(page.locator(".hex-view")).toHaveCount(0);
  expect(
    (await page.locator(".decode-area").boundingBox())!.width,
  ).toBeGreaterThan(width + 300);
  await page.getByRole("button", { name: "展开原始字节" }).click();
  await expect(page.locator(".hex-view")).toBeVisible();
  await page.getByRole("button", { name: "更多操作" }).click();
  await page.getByRole("menuitem", { name: "弹窗查看" }).click();
  const popup = page.getByRole("dialog", { name: "包详情弹窗" });
  await expect(
    popup.getByRole("heading", { name: "Test.SearchTarget" }),
  ).toBeVisible();
  await page.screenshot({
    path: resolve(root, "packet-popup.png"),
    fullPage: true,
  });
  await page.keyboard.press("Escape");
  await expect(popup).toHaveCount(0);
  await page
    .getByRole("option", { name: "选择包 17: Test.SearchTarget" })
    .click({ button: "right" });
  await page.getByRole("menuitem", { name: "弹窗查看" }).click();
  await expect(popup).toBeVisible();
  await popup.getByRole("button", { name: "关闭弹窗" }).click();
});

test("advanced filters select historical packets and multi-compare three real payloads", async ({
  page,
  request,
}) => {
  const id = await sessionId(request, "closed.qscap");
  await page.goto(`/?session=${id}`);
  await page.getByRole("button", { name: "高级筛选", exact: true }).click();
  await page
    .getByRole("textbox", { name: "起始事件 ID", exact: true })
    .fill("17");
  await page
    .getByRole("textbox", { name: "结束事件 ID", exact: true })
    .fill("19");
  await page.getByRole("button", { name: "应用筛选", exact: true }).click();
  const list = page.getByRole("listbox", { name: "数据包列表" });
  await expect(list.getByRole("option")).toHaveCount(3);
  await expect(
    page.getByRole("checkbox", { name: "对比选择 17", exact: true }),
  ).toHaveCount(0);
  await page.getByRole("button", { name: "对比模式", exact: true }).click();
  for (const id of [17, 18, 19])
    await page
      .getByRole("checkbox", { name: `对比选择 ${id}`, exact: true })
      .check();
  await page.getByRole("button", { name: "对比 3", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "多包对比 · 3 个包" });
  await expect(
    dialog.locator(".comparison-grid .difference").first(),
  ).toBeVisible();
  const baseline = dialog.getByRole("combobox", { name: "对比基准" });
  await expect(baseline).toContainText("#17");
  await baseline.click();
  await expect(baseline).toBeFocused();
  await expect(
    dialog.getByRole("listbox", { name: "对比基准" }).getByRole("option"),
  ).toHaveCount(3);
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("Enter");
  await expect(baseline).toContainText("#18");
  await expect(baseline).toBeFocused();
  await baseline.press("ArrowDown");
  await page.keyboard.press("Escape");
  await expect(dialog).toBeVisible();
  await expect(dialog.getByRole("listbox", { name: "对比基准" })).toHaveCount(
    0,
  );
  await dialog.getByRole("button", { name: "字节", exact: true }).click();
  await expect(dialog.getByLabel("字节对比")).toBeVisible();
  expect(await dialog.locator(".comparison-row").count()).toBeLessThan(100);
  await page.screenshot({
    path: resolve(root, "multi-compare.png"),
    fullPage: true,
  });
  await page.keyboard.press("Escape");
  await page.getByRole("button", { name: "命令筛选", exact: true }).click();
  const commands = page.getByRole("dialog", { name: "命令筛选" });
  await commands
    .getByRole("button", { name: "排除 Test.TLV", exact: true })
    .click();
  await commands
    .getByRole("textbox", { name: "查找命令或输入规则" })
    .fill("=Test.JCE");
  await page.keyboard.press("Shift+Enter");
  await expect(list.getByRole("option")).toHaveCount(1);
  await expect(list.getByRole("option")).toContainText("SearchTarget");
  await expect(
    commands.getByRole("button", { name: "排除 Test.JCE", exact: true }),
  ).toHaveAttribute("aria-pressed", "true");
});

test("jump to latest keeps packet DOM bounded and directory warnings stay in the footer", async ({
  page,
  request,
}) => {
  await writeFile(join(root, "captures/invalid.qscap"), "synthetic-invalid");
  const id = await sessionId(request, "closed.qscap");
  await page.goto(`/?session=${id}`);
  await expect(
    page.getByRole("button", { name: "查看文件加载提示" }),
  ).toBeVisible();
  await expect(page.locator(".warning-banner")).toHaveCount(0);
  await page.getByRole("button", { name: "查看文件加载提示" }).click();
  await expect(
    page.getByRole("dialog", { name: "文件加载提示" }),
  ).toContainText("invalid.qscap");
  await page.getByRole("button", { name: "收起文件加载提示" }).click();
  const list = page.getByRole("listbox", { name: "数据包列表" });
  expect(await list.getByRole("option").count()).toBeLessThan(80);
  await list.evaluate((element) => {
    element.scrollTop = 0;
  });
  await page.getByRole("button", { name: "滚动到最新记录" }).click();
  await expect(
    page.getByRole("option", { name: "选择包 1200: Test.Command.0" }),
  ).toBeVisible();
  expect(await list.getByRole("option").count()).toBeLessThan(80);
});

test("record button saves only the chosen live window and opens it as an independent history file", async ({
  page,
  request,
}) => {
  const id = await sessionId(request, "live.qscap");
  // A synthetic producer has no QQ process. Override only its UI liveness metadata.
  await page.route("**/api/v1/sessions**", async (route) => {
    const response = await route.fetch();
    if (route.request().method() !== "GET") {
      await route.fulfill({ response });
      return;
    }
    const body = await response.json();
    if (body.sessions) {
      for (const s of body.sessions) if (s.id === id) s.producer_alive = true;
    }
    if (body.session?.id === id) body.session.producer_alive = true;
    await route.fulfill({ response, json: body });
  });
  await page.goto(`/?session=${id}`);
  await page.getByRole("button", { name: "开始录制", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "停止录制", exact: true }),
  ).toBeVisible();
  await page.route("**/api/v1/recordings/*/stop", async (route) => {
    const response = await route.fetch();
    const jobs = await response.json();
    for (const job of jobs) job.state = "recording";
    await route.fulfill({ response, json: jobs });
  });
  await appendFile(
    join(root, "captures/live.qscap"),
    record(4, "Live.Recorded", Buffer.from([8, 42])),
  );
  await expect(
    page.getByRole("button", { name: "停止录制", exact: true }),
  ).toContainText("1 包");
  await page.getByRole("button", { name: "冻结", exact: true }).click();
  await page.getByRole("button", { name: "停止录制", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "Live.Recorded", exact: true }),
  ).toBeVisible({ timeout: 15_000 });
  const records = await request.get("/api/v1/recordings", {
    headers: { "X-QScope-Client": "web-v1" },
  });
  const jobs = await records.json();
  expect(jobs.at(-1).packets).toBe("1");
  expect(jobs.at(-1).state).toBe("stopped");
  await expect(
    page.getByRole("listbox", { name: "数据包列表" }).getByRole("option"),
  ).toHaveCount(1);
  await page.unrouteAll({ behavior: "wait" });
});

test("JSON layout, Inter, solid buttons and full Unicode value copying never use the truncated preview", async ({
  page,
  request,
  context,
}) => {
  await grantClipboard(context);
  const id = await sessionId(request, "text.qscap");
  await page.goto(`/?session=${id}`);
  await page
    .getByRole("option", { name: "选择包 1: Long.Text", exact: true })
    .click();
  const row = page.getByRole("treeitem").first();
  await expect(row.locator(".json-key")).toHaveText('"1"');
  await expect(row.locator(".json-value")).toContainText("前缀");
  await expect(row.locator(".json-field-meta")).toContainText("bytes");
  expect(
    await row
      .locator(".json-value")
      .evaluate((element) => getComputedStyle(element).userSelect),
  ).toBe("text");
  await page.evaluate(() => document.fonts.ready);
  expect(
    await page
      .locator(".json-value")
      .evaluate((element) => getComputedStyle(element).fontFamily),
  ).toContain("Inter");
  expect(
    await page.evaluate(() =>
      [...document.fonts].some(
        (face) => face.family === "Inter" && face.status === "loaded",
      ),
    ),
  ).toBeTruthy();
  const copy = page.getByRole("button", {
    name: "复制 JSON 结构",
    exact: true,
  });
  expect(
    await copy.evaluate((element) => getComputedStyle(element).backgroundColor),
  ).not.toBe("rgba(0, 0, 0, 0)");
  expect(
    await copy.evaluate((element) =>
      parseFloat(getComputedStyle(element).borderTopWidth),
    ),
  ).toBeGreaterThan(0);
  await row
    .getByRole("button", { name: /^字段 1 · bytes:/ })
    .click({ button: "right" });
  await page.getByRole("menuitem", { name: "复制值", exact: true }).click();
  await expect
    .poll(() => page.evaluate(() => navigator.clipboard.readText()))
    .toBe(longText);
  // The line stays one row high; double-clicking the value selects all of it without the quotes.
  expect(
    await row
      .locator(".json-field-row")
      .evaluate((element) => element.getBoundingClientRect().height),
  ).toBe(22);
  await row.locator(".json-value").dblclick();
  expect(
    await page.evaluate(() => window.getSelection()?.toString() || ""),
  ).toBe(await row.locator(".json-text").textContent());
  expect(
    await page.evaluate(() =>
      window.getSelection()?.toString().startsWith("前缀"),
    ),
  ).toBe(true);
  // Hover actions beside the value copy it and open the compact reader.
  await page.evaluate(() => navigator.clipboard.writeText(""));
  await row.hover();
  await row.getByRole("button", { name: "复制值", exact: true }).click();
  await expect
    .poll(() => page.evaluate(() => navigator.clipboard.readText()))
    .toBe(longText);
  await row.hover();
  await row.getByRole("button", { name: "查看完整值", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "字段 1 · 完整值" });
  await expect(dialog.getByRole("textbox", { name: "完整字段值" })).toHaveValue(
    longText,
  );
  expect((await dialog.boundingBox())!.width).toBeLessThanOrEqual(620);
  await expect(
    dialog.getByRole("tablist", { name: "值的显示方式" }),
  ).toHaveCount(0);
  await dialog.getByRole("button", { name: "复制完整值", exact: true }).click();
  await expect
    .poll(() => page.evaluate(() => navigator.clipboard.readText()))
    .toBe(longText);
  await page.screenshot({
    path: resolve(root, "full-value.png"),
    fullPage: true,
  });
  await page.keyboard.press("Escape");
  await page.evaluate(() => navigator.clipboard.writeText(""));
  await copy.click();
  await expect
    .poll(async () => (await clipboardJson(page))?.["1"])
    .toBe(longText);
  await page.screenshot({
    path: resolve(root, "json-detail.png"),
    fullPage: true,
  });
});

test("a JSON value opens formatted and colored with exact integers, and the original text stays one tab away", async ({
  page,
  request,
  context,
}) => {
  await grantClipboard(context);
  const name = JSON.stringify('示例 "引号"');
  const raw = `{"id":"demo","seq":18446744073709551615,"ok":true,"none":null,"list":[1,-2.5e3,{"name":${name},"tags":[]}],"empty":{}}`;
  const pretty = [
    "{",
    '  "id": "demo",',
    '  "seq": 18446744073709551615,',
    '  "ok": true,',
    '  "none": null,',
    '  "list": [',
    "    1,",
    "    -2.5e3,",
    "    {",
    `      "name": ${name},`,
    '      "tags": []',
    "    }",
    "  ],",
    '  "empty": {}',
    "}",
  ].join("\n");
  const id = await sessionId(request, "text.qscap");
  await page.goto(`/?session=${id}`);
  await page
    .getByRole("option", { name: "选择包 4: Json.Text", exact: true })
    .click();
  const row = page.getByRole("treeitem").first();
  await row.hover();
  await row.getByRole("button", { name: "查看完整值", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "字段 1 · 完整值" });
  await expect(dialog.getByRole("tab", { name: "JSON" })).toHaveAttribute(
    "aria-selected",
    "true",
  );
  const reader = dialog.getByRole("region", { name: "格式化 JSON" });
  await expect(reader.locator(".json-reader-line")).toHaveCount(15);
  await expect(reader.locator(".json-token-key").first()).toHaveText('"id"');
  await expect(reader.locator(".json-token-number").first()).toHaveText(
    "18446744073709551615",
  );
  await expect(reader.locator(".json-token-literal")).toHaveText([
    "true",
    "null",
  ]);
  expect(
    await reader
      .locator(".json-token-key")
      .first()
      .evaluate((element) => getComputedStyle(element).color),
  ).not.toBe(
    await reader
      .locator(".json-token-string")
      .first()
      .evaluate((element) => getComputedStyle(element).color),
  );
  await dialog.getByRole("button", { name: "复制格式化", exact: true }).click();
  await expect.poll(() => clipboard(page)).toBe(pretty);
  await dialog.getByRole("button", { name: "复制完整值", exact: true }).click();
  await expect
    .poll(() => page.evaluate(() => navigator.clipboard.readText()))
    .toBe(raw);
  await dialog.getByRole("tab", { name: "原文" }).click();
  await expect(dialog.getByRole("textbox", { name: "完整字段值" })).toHaveValue(
    raw,
  );
  await page.screenshot({ path: resolve(root, "json-value.png") });
});

test("a large field view opens fully, scrolls virtually and streams later pages into nested fields", async ({
  page,
  request,
}) => {
  const id = await sessionId(request, "text.qscap");
  const pages: string[] = [];
  page.on("request", (item) => {
    if (/\/events\/3\?/.test(item.url()))
      pages.push(new URL(item.url()).searchParams.get("node_offset") || "0");
  });
  await page.goto(`/?session=${id}`);
  await page
    .getByRole("option", { name: "选择包 3: Nested.Pages", exact: true })
    .click();
  const tree = page.getByRole("tree", { name: "解码字段树" });
  await expect(tree.locator('.field-row[data-path="1[0]"]')).toBeVisible();
  // Only the lines near the viewport exist in the page.
  expect(await tree.getByRole("treeitem").count()).toBeLessThan(120);
  const scroll = page.locator(".tree-scroll");
  await scroll.evaluate((element) => {
    element.scrollTop = element.scrollHeight;
  });
  await expect.poll(() => pages).toEqual(["0", "2000"]);
  await scroll.evaluate((element) => {
    element.scrollTop = element.scrollHeight;
  });
  await expect(tree.locator('.field-row[data-path="2.1[599]"]')).toBeVisible();
  await expect(tree.locator(".json-more")).toHaveCount(0);
  await expect(page.locator(".decode-badge.warning")).toHaveCount(0);
  expect(await tree.getByRole("treeitem").count()).toBeLessThan(120);
});

test("JSON copying groups repeats without dropping fields and preserves all 64-bit digits", async ({
  page,
  request,
  context,
}) => {
  await grantClipboard(context);
  const id = await sessionId(request, "text.qscap");
  await page.goto(`/?session=${id}`);
  await page
    .getByRole("option", { name: "选择包 2: Repeated.Fields", exact: true })
    .click();
  await page
    .getByRole("button", { name: "复制 JSON 结构", exact: true })
    .click();
  await expect
    .poll(() => clipboardJson(page))
    .toEqual({ "1": ["1", "2"], "2": "18446744073709551615" });
});

test("loading indicators stay circular and bounded in the main inspector and detail popup", async ({
  page,
  request,
}) => {
  const id = await sessionId(request, "closed.qscap");
  let release!: () => void;
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route(
    /\/api\/v1\/sessions\/[^/]+\/events\/\d+\?/,
    async (route) => {
      await gate;
      await route.continue();
    },
  );
  try {
    await page.goto(`/?session=${id}`);
    const spinner = page.locator(".small-spinner");
    await expect(spinner).toBeVisible();
    const assertBounded = async () => {
      for (const element of await spinner.all()) {
        const style = await element.evaluate((node) => {
          const css = getComputedStyle(node);
          return {
            width: parseFloat(css.width),
            height: parseFloat(css.height),
            grow: css.flexGrow,
          };
        });
        expect(style.width).toBe(12);
        expect(style.height).toBe(12);
        expect(style.grow).toBe("0");
        const box = await element.boundingBox();
        expect(box!.width).toBeLessThanOrEqual(18);
        expect(box!.height).toBeLessThanOrEqual(18);
      }
    };
    await assertBounded();
    await page.screenshot({
      path: resolve(root, "loading-spinner.png"),
      fullPage: true,
    });
    await page.getByRole("button", { name: "更多操作" }).click();
    await page.getByRole("menuitem", { name: "弹窗查看" }).click();
    await expect(spinner).toHaveCount(2);
    await assertBounded();
    await page.screenshot({
      path: resolve(root, "loading-spinner-popup.png"),
      fullPage: true,
    });
    await page.keyboard.press("Escape");
  } finally {
    release();
    await page.unrouteAll({ behavior: "wait" });
  }
  await expect(page.locator(".small-spinner")).toHaveCount(0);
});

test("a restored capture renders before the workspace catalogue finishes loading", async ({
  page,
  request,
}) => {
  const id = await sessionId(request, "closed.qscap");
  let release!: () => void;
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route(/\/api\/v1\/sessions$/, async (route) => {
    await gate;
    await route.continue();
  });
  try {
    await page.goto(`/?session=${id}`);
    await expect(
      page.getByRole("heading", { name: "Test.Command.0", exact: true }),
    ).toBeVisible({ timeout: 2500 });
    await expect(page.getByRole("tree", { name: "解码字段树" })).toBeVisible();
    await expect(
      page.getByRole("tab", { name: /未识别账号.*PID 123/ }),
    ).toBeVisible();
    await expect(page.getByText("正在读取工作区", { exact: true })).toHaveCount(
      0,
    );
    await page.screenshot({
      path: resolve(root, "workspace-before-catalog.png"),
      fullPage: true,
    });
  } finally {
    release();
    await page.unrouteAll({ behavior: "wait" });
  }
});

test("settings install each agent into the selected project, copy the universal prompt and preserve custom skills", async ({
  page,
  request,
  context,
}) => {
  await grantClipboard(context);
  const id = await sessionId(request, "closed.qscap");
  await page.goto(`/?session=${id}`);
  await page.getByRole("button", { name: "打开设置", exact: true }).click();
  const settings = page.getByRole("dialog", { name: "QScope 设置" });
  await expect(settings).toBeVisible();
  await expect(page).toHaveURL(/page=settings/);
  await expect(settings.getByRole("tab", { name: /QQ 抓包/ })).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await settings.getByRole("tab", { name: /QQ 抓包/ }).press("ArrowDown");
  await expect(settings.getByRole("tab", { name: /AI 接入/ })).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await expect(page).toHaveURL(/section=ai/);
  await expect(
    settings.getByRole("heading", { name: "安装范围与路径" }),
  ).toBeVisible();
  await expect(page.getByLabel("项目目录", { exact: true })).toHaveValue(root);
  for (const [agent, directory] of [
    ["Claude Code", ".claude"],
    ["Codex", ".agents"],
    ["Cursor", ".cursor"],
  ]) {
    const button = page.getByRole("button", {
      name: `安装到 ${agent}`,
      exact: true,
    });
    await button.click();
    await expect(button).toHaveText("已安装");
    const content = await readFile(
      join(root, directory, "skills/qscope/SKILL.md"),
      "utf8",
    );
    expect(content.startsWith("---\nname: qscope\n")).toBeTruthy();
    expect(content).toContain("live record start SESSION");
    expect(content).toContain("任何金融相关任务直接拒绝");
    expect(content).toContain("http://127.0.0.1:19841");
    expect(content).not.toContain("{{CLI_PATH}}");
  }
  await page
    .getByRole("button", { name: "复制通用安装指令", exact: true })
    .click();
  const claudeSettings = JSON.parse(
    await readFile(join(root, ".claude/settings.local.json"), "utf8"),
  );
  expect(
    claudeSettings.permissions.allow.some(
      (rule: string) =>
        rule.startsWith("PowerShell(& ") &&
        rule.includes(
          process.platform === "win32" ? "qscope-cli.exe" : "qscope-cli",
        ),
    ),
  ).toBeTruthy();
  for (const [agent, directory] of [
    ["Claude Code", ".claude"],
    ["Codex", ".agents"],
    ["Cursor", ".cursor"],
  ]) {
    await page
      .getByRole("button", { name: `卸载 ${agent} 技能`, exact: true })
      .click();
    await expect(
      page.getByRole("button", { name: `安装到 ${agent}`, exact: true }),
    ).toHaveText("安装");
    await expect(
      access(join(root, directory, "skills/qscope/SKILL.md")),
    ).rejects.toMatchObject({ code: "ENOENT" });
  }
  expect(
    JSON.parse(
      await readFile(join(root, ".claude/settings.local.json"), "utf8"),
    ).permissions.allow,
  ).toHaveLength(0);
  for (const agent of ["Claude Code", "Codex", "Cursor"]) {
    const button = page.getByRole("button", {
      name: `安装到 ${agent}`,
      exact: true,
    });
    await button.click();
    await expect(button).toHaveText("已安装");
  }
  const prompt = await page
    .getByLabel("通用安装指令", { exact: true })
    .inputValue();
  await expect
    .poll(() => page.evaluate(() => navigator.clipboard.readText()))
    .toBe(prompt);
  expect(prompt).toContain("claude-code、codex、cursor");
  const skillPath = join(root, ".agents/skills/qscope/SKILL.md");
  const custom = (await readFile(skillPath, "utf8")).replace(
    "采集与定位",
    "我的自定义步骤",
  );
  await writeFile(skillPath, custom);
  await page.reload();
  await expect(page.getByText("内容有改动", { exact: true })).toBeVisible();
  await expect(
    page.getByRole("button", { name: "安装到 Codex", exact: true }),
  ).toBeDisabled();
  expect(await readFile(skillPath, "utf8")).toBe(custom);
  await page.screenshot({
    path: resolve(root, "settings-integrations.png"),
    fullPage: true,
  });
  await page.setViewportSize({ width: 480, height: 900 });
  await page
    .getByRole("button", { name: "复制通用安装指令", exact: true })
    .scrollIntoViewIfNeeded();
  expect(
    await page.evaluate(() => document.documentElement.scrollWidth),
  ).toBeLessThanOrEqual(480);
  await page.screenshot({
    path: resolve(root, "settings-narrow.png"),
    fullPage: true,
  });
  await settings.getByRole("button", { name: "关闭弹窗", exact: true }).click();
  await expect(settings).toHaveCount(0);
  await expect(page.getByRole("main", { name: "抓包工作区" })).toBeVisible();
  await expect(page).toHaveURL(new RegExp(id));
  await expect(page).not.toHaveURL(/page=settings|section=/);
});

test("settings reopen on the skill scope and project directory last chosen", async ({
  page,
}) => {
  await page.goto("/?page=settings&section=ai");
  const scope = page.getByRole("group", { name: "技能安装范围" });
  const global = scope.getByRole("button", { name: "全局", exact: true });
  const project = scope.getByRole("button", { name: "当前项目", exact: true });
  await expect(page.getByLabel("项目目录", { exact: true })).toHaveValue(root);
  await global.click();
  await expect(global).toHaveAttribute("aria-pressed", "true");
  await expect(page.getByText("用户目录", { exact: true })).toBeVisible();
  await page.reload();
  await expect(global).toHaveAttribute("aria-pressed", "true");
  await project.click();
  await expect(page.getByLabel("项目目录", { exact: true })).toHaveValue(root);
  await page.reload();
  await expect(project).toHaveAttribute("aria-pressed", "true");
  await expect(page.getByLabel("项目目录", { exact: true })).toHaveValue(root);
  await expect(
    page.getByRole("list", { name: "AI 工具" }).getByRole("listitem"),
  ).not.toHaveCount(0);
});

test("CLI queries, decodes, compares and records through the running workbench with exact 64-bit strings", async ({
  request,
}) => {
  const cliPath = resolve(
    import.meta.dirname,
    "../../../target/debug",
    process.platform === "win32" ? "qscope-cli.exe" : "qscope-cli",
  );
  const execute = promisify(execFile);
  async function cli(...argumentsList: string[]) {
    const { stdout } = await execute(
      cliPath,
      [
        "--json",
        "live",
        "--server",
        "http://127.0.0.1:19841",
        ...argumentsList,
      ],
      { windowsHide: true, maxBuffer: 4 * 1024 * 1024 },
    );
    return JSON.parse(stdout);
  }
  const closed = await sessionId(request, "closed.qscap");
  await writeFile(
    join(root, "captures/cli-live.qscap"),
    Buffer.concat([
      header(987),
      record(1, "CLI.Initial", Buffer.from([8, 1]), { pid: 987 }),
    ]),
  );
  const live = await sessionId(request, "cli-live.qscap");
  const catalog = await cli("sessions");
  expect(
    catalog.sessions.some((session: { id: string }) => session.id === closed),
  ).toBeTruthy();
  const page = await cli(
    "events",
    closed,
    "--q",
    "SearchTarget",
    "--limit",
    "2",
  );
  expect(page.events[0].sequence).toBe("9007199254741010");
  const packet = await cli("packet", closed, "17");
  expect(packet.event.event_id).toBe("17");
  expect(packet.nodes.length).toBeGreaterThan(0);
  const comparison = await cli(
    "compare",
    "--packet",
    `${closed}:17`,
    "--packet",
    `${closed}:18`,
  );
  expect(comparison.columns).toHaveLength(2);
  const started = await cli("record", "start", live);
  await appendFile(
    join(root, "captures/cli-live.qscap"),
    record(2, "CLI.Recorded", Buffer.from([8, 53]), { pid: 987 }),
  );
  const matched = await cli(
    "wait",
    live,
    "--after",
    "1",
    "--command",
    "CLI.Recorded",
    "--timeout",
    "5",
  );
  expect(matched.matched).toBeTruthy();
  expect(matched.page.events[0].event_id).toBe("2");
  await expect
    .poll(async () => {
      const jobs = await cli("record", "list");
      return jobs.find((job: { id: string }) => job.id === started.id)?.packets;
    })
    .toBe("1");
  await cli("record", "stop", started.id);
  await expect
    .poll(async () => {
      const jobs = await cli("record", "list");
      return jobs.find((job: { id: string }) => job.id === started.id)?.state;
    })
    .toBe("stopped");
});

test("one DevTools button opens the current visible target without connection or window selectors", async ({
  page,
}) => {
  await page.route("**/api/v1/settings/qq", (route) =>
    route.fulfill({
      json: {
        build: "9.9.37-54395",
        stage: "running",
        installed: true,
        available: true,
        message: "QQ 已运行",
        downloaded: "0",
        total: "334442672",
        pid: 6789,
        session_id: null,
        error: null,
      },
    }),
  );
  let opened: unknown;
  await page.route("**/api/v1/settings/devtools/current/open", (route) => {
    opened = route.request().postDataJSON();
    return route.fulfill({
      json: {
        opened: true,
        frontend_url: "http://127.0.0.1:19841/__synthetic-inspector",
        frontend: "Chii 1.15.5",
      },
    });
  });
  await page.route("**/__synthetic-inspector", (route) =>
    route.fulfill({
      contentType: "text/html",
      body: "<!doctype html><title>Synthetic inspector</title><p>Inspector connected</p>",
    }),
  );
  await page.goto("/?page=settings");
  await expect(
    page.getByRole("button", { name: "检测连接", exact: true }),
  ).toHaveCount(0);
  await expect(page.getByLabel("目标窗口", { exact: true })).toHaveCount(0);
  await page
    .getByRole("button", { name: "打开 DevTools", exact: true })
    .click();
  const dialog = page.getByRole("dialog", {
    name: "QQ DevTools · Chii",
    exact: true,
  });
  await expect(dialog).toBeVisible();
  await expect(
    page
      .frameLocator('iframe[title="QQ DevTools"]')
      .getByText("Inspector connected"),
  ).toBeVisible();
  expect(opened).toEqual({});
  await dialog.getByRole("button", { name: "关闭弹窗", exact: true }).click();
  await expect(dialog).not.toBeVisible();
  await expect(page.getByRole("dialog", { name: "QScope 设置" })).toBeVisible();
});

test("a running research QQ can be closed from the settings", async ({
  page,
  request,
}) => {
  let stage = "running";
  let stops: unknown[] = [];
  const setup = () => ({
    build: "9.9.37-54395",
    stage,
    installed: true,
    available: true,
    message: stage === "running" ? "QQ 已运行，流量正在记录" : "未运行",
    downloaded: "0",
    total: "334442672",
    pid: stage === "running" ? 6789 : null,
    session_id: null,
    error: null,
  });
  await page.route("**/api/v1/settings/qq", (route) =>
    route.fulfill({ json: setup() }),
  );
  await page.route("**/api/v1/settings/qq/stop", (route) => {
    stops = [...stops, route.request().postDataJSON()];
    stage = "idle";
    return route.fulfill({ json: setup() });
  });
  await page.goto("/?page=settings");
  await expect(page.getByText("PID 6789")).toBeVisible();
  await page.getByRole("button", { name: "关闭 QQ", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "启动 QQ", exact: true }),
  ).toBeEnabled();
  await expect(
    page.getByRole("button", { name: "关闭 QQ", exact: true }),
  ).toHaveCount(0);
  expect(stops).toEqual([{}]);

  // With nothing running the real service only reports the current state.
  const idle = await request.post("/api/v1/settings/qq/stop", {
    headers: { "X-QScope-Client": "web-v1" },
    data: { page_only: true },
  });
  expect(idle.ok()).toBeTruthy();
  expect((await idle.json()).stage).not.toBe("running");
});

test("first run prepares QQ with one button, shows progress and opens its capture", async ({
  page,
  request,
}) => {
  const id = await sessionId(request, "closed.qscap");
  let stage = "idle";
  let posts = 0;
  await page.route("**/api/v1/settings/qq", (route) => {
    if (route.request().method() === "POST") {
      posts++;
      stage = "downloading";
    }
    return route.fulfill({
      json: {
        build: "9.9.37-54395",
        stage,
        installed: stage !== "idle",
        available: true,
        message: "正在下载官方 QQ 安装包",
        downloaded: "83886080",
        total: "334442672",
        pid: stage === "running" ? 123 : null,
        session_id: stage === "running" ? id : null,
        error: null,
      },
    });
  });
  await page.goto("/?page=settings");
  await page
    .getByRole("button", { name: "下载并启动 QQ", exact: true })
    .click();
  await expect(page.getByRole("progressbar")).toBeVisible();
  await expect(
    page.getByRole("button", { name: "正在下载官方 QQ 安装包", exact: true }),
  ).toBeDisabled();
  stage = "running";
  await expect(page.getByRole("dialog", { name: "QScope 设置" })).toHaveCount(
    0,
  );
  await expect(page.getByRole("main", { name: "抓包工作区" })).toBeVisible();
  await expect(page).toHaveURL(new RegExp(id));
  expect(posts).toBe(1);
});

test("QQ preparation failure exposes a retry while keeping the simplified settings", async ({
  page,
}) => {
  await page.route("**/api/v1/settings/qq", (route) =>
    route.fulfill({
      json: {
        build: "9.9.37-54395",
        stage: "error",
        installed: false,
        available: true,
        message: "准备失败",
        downloaded: "0",
        total: "334442672",
        pid: null,
        session_id: null,
        error: "QQ 下载签名接口暂时不可用",
      },
    }),
  );
  await page.goto("/?page=settings");
  await expect(page.getByRole("alert")).toContainText(
    "QQ 下载签名接口暂时不可用",
  );
  await expect(
    page.getByRole("button", { name: "下载并启动 QQ", exact: true }),
  ).toBeEnabled();
  await expect(
    page.getByLabel("DevTools 控制端口", { exact: true }),
  ).toHaveCount(0);
  await expect(page.getByLabel("调试 profile", { exact: true })).toHaveCount(0);
});

test("launches of one account share a masked tab, reveal on demand and merge with launch boundaries", async ({
  page,
  request,
}) => {
  for (const name of [
    "account-first.qscap",
    "account-second.qscap",
    "account-other.qscap",
  ])
    await accountOf(request, name);
  const first = await sessionId(request, "account-first.qscap");
  const second = await sessionId(request, "account-second.qscap");
  await page.goto(`/?session=${second}`);
  const tabs = page.getByRole("tablist", { name: "采集记录快捷切换" });
  const tab = tabs.getByRole("tab", { selected: true });
  await expect(tab).toContainText("1***1");
  await expect(tab).toContainText("待核验");
  await expect(tab).toContainText("PID 302");
  await expect(page.locator("body")).not.toContainText("10001");
  await page.getByRole("button", { name: "选择1***1的启动记录" }).click();
  const launches = page.getByRole("menu", { name: "1***1 的启动记录" });
  await expect(launches.getByRole("menuitemradio")).toHaveCount(3);
  await expect(
    launches.getByRole("menuitemradio", { name: /^打开第 2 次启动 PID 302 / }),
  ).toHaveAttribute("aria-checked", "true");
  await launches
    .getByRole("menuitemradio", { name: /^打开第 1 次启动 PID 301 / })
    .click();
  await expect(page).toHaveURL(new RegExp(first));
  await expect(tabs.getByRole("tab")).toHaveCount(1);
  await expect(
    page.getByRole("option", { name: "选择包 7: First.Command.2" }),
  ).toBeVisible();
  await page.getByRole("button", { name: "选择1***1的启动记录" }).click();
  await launches
    .getByRole("menuitemradio", { name: /合并查看全部 2 次启动/ })
    .click();
  await expect(page).toHaveURL(/merged=1/);
  await expect(tab).toContainText("合并 2 次启动");
  const list = page.getByRole("listbox", { name: "数据包列表" });
  await expect(list.getByRole("option")).toHaveCount(10);
  await expect(list.locator(".packet-boundary")).toHaveCount(2);
  await expect(list.locator(".packet-boundary").first()).toContainText(
    "第 1 次启动",
  );
  await expect(list.locator(".packet-boundary").nth(1)).toContainText(
    "PID 302",
  );
  // Event IDs repeat across launches; each row keeps its own launch identity.
  await list.getByRole("option", { name: "选择包 2: First.Command.0" }).click();
  await expect(page.getByRole("region", { name: "包详情" })).toContainText(
    "第 1 次启动 · PID 301",
  );
  await list
    .getByRole("option", { name: "选择包 2: Second.Command.0" })
    .click();
  await expect(page.getByRole("region", { name: "包详情" })).toContainText(
    "第 2 次启动 · PID 302",
  );
  await tab.click();
  await expect(launches).toBeVisible();
  await page.getByRole("button", { name: "显示完整账号" }).click();
  await expect(tab).toContainText("10001");
  await page.reload();
  await expect(tabs.getByRole("tab", { selected: true })).toContainText(
    "10001",
  );
  await page.getByRole("button", { name: "选择10001的启动记录" }).click();
  await page.getByRole("button", { name: "隐藏完整账号" }).click();
  await expect(tab).toContainText("1***1");
  await expect(page.locator("body")).not.toContainText("10001");
  await page.getByRole("button", { name: /^历史记录/ }).click();
  const history = page.getByRole("dialog", { name: "历史抓包" });
  await expect(
    history.getByRole("region", { name: "账号 1***1" }),
  ).toContainText("2 次启动");
  await expect(
    history.getByRole("region", { name: "账号 1***2" }),
  ).toContainText("1 次启动");
  await expect(
    history.getByRole("region", { name: "未识别账号" }),
  ).toContainText("QQ 启动");
  await page.evaluate(() => document.fonts.ready);
  await page.screenshot({
    path: resolve(root, "account-history.png"),
    fullPage: true,
  });
});

test("the list/detail splitter starts at 30%, resizes by pointer and keyboard and is remembered", async ({
  page,
  request,
}) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const id = await sessionId(request, "closed.qscap");
  await page.goto(`/?session=${id}`);
  const inspector = page.locator(".inspector");
  const list = page.getByRole("region", { name: "数据包面板" });
  const ratio = async () =>
    (await list.boundingBox())!.width / (await inspector.boundingBox())!.width;
  await expect(list).toBeVisible();
  expect(await ratio()).toBeCloseTo(0.3, 1);
  const splitter = page.getByRole("separator", { name: "调整列表与详情宽度" });
  const area = (await inspector.boundingBox())!;
  const handle = (await splitter.boundingBox())!;
  await page.mouse.move(handle.x + handle.width / 2, handle.y + 300);
  await page.mouse.down();
  await page.mouse.move(area.x + area.width * 0.5, handle.y + 300, {
    steps: 6,
  });
  await page.mouse.up();
  expect(await ratio()).toBeCloseTo(0.5, 1);
  await page.reload();
  await expect(list).toBeVisible();
  expect(await ratio()).toBeCloseTo(0.5, 1);
  await splitter.focus();
  await page.keyboard.press("ArrowLeft");
  await expect(splitter).toHaveAttribute("aria-valuenow", "48");
  await page.keyboard.press("Home");
  expect((await list.boundingBox())!.width).toBeGreaterThanOrEqual(299);
  await page.keyboard.press("End");
  expect(
    (await page.getByRole("region", { name: "包详情" }).boundingBox())!.width,
  ).toBeGreaterThanOrEqual(519);
  await page.keyboard.press("Enter");
  await expect(splitter).toHaveAttribute("aria-valuenow", "30");
  // Blocked storage only loses the remembered width.
  await page.addInitScript(() => {
    Storage.prototype.getItem = () => {
      throw new Error("blocked");
    };
    Storage.prototype.setItem = () => {
      throw new Error("blocked");
    };
  });
  await page.reload();
  await expect(list).toBeVisible();
  expect(await ratio()).toBeCloseTo(0.3, 1);
  await splitter.focus();
  await page.keyboard.press("ArrowRight");
  await expect(splitter).toHaveAttribute("aria-valuenow", "32");
  expect(errors).toEqual([]);
});

test("keyboard moves the selection through the list and the detail follows only the chosen row", async ({
  page,
  request,
}) => {
  const id = await sessionId(request, "closed.qscap");
  await page.goto(`/?session=${id}`);
  const list = page.getByRole("listbox", { name: "数据包列表" });
  await expect(
    page.getByRole("heading", { name: "Test.Command.0" }),
  ).toBeVisible();
  await list
    .getByRole("option", { name: "选择包 1198: Test.Command.18" })
    .click();
  await expect(
    page.getByRole("heading", { name: "Test.Command.18" }),
  ).toBeVisible();
  await page.keyboard.press("ArrowUp");
  await page.keyboard.press("ArrowUp");
  await expect(
    page.getByRole("heading", { name: "Test.Command.16" }),
  ).toBeVisible();
  await expect(list).toBeFocused();
  await expect(list).toHaveAttribute("aria-activedescendant", /-1196$/);
  await expect(
    list.getByRole("option", { name: "选择包 1196: Test.Command.16" }),
  ).toHaveAttribute("aria-selected", "true");
  await page.keyboard.press("End");
  await expect(
    page.getByRole("heading", { name: "Test.Command.0" }),
  ).toBeVisible();
  await page.keyboard.press("Space");
  await expect(page.getByRole("region", { name: "对比选择" })).toContainText(
    "已勾选 1",
  );
  await expect(
    page.getByRole("checkbox", { name: "对比选择 1200" }),
  ).toBeChecked();
  await page.keyboard.press("Shift+Enter");
  await expect(page.getByRole("dialog", { name: "包详情弹窗" })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog", { name: "包详情弹窗" })).toHaveCount(0);
});

test("row context menu works by mouse and keyboard, never changes the detail itself and returns focus", async ({
  page,
  request,
  context,
}) => {
  await grantClipboard(context);
  const id = await sessionId(request, "closed.qscap");
  await page.goto(`/?session=${id}`);
  const list = page.getByRole("listbox", { name: "数据包列表" });
  await expect(
    page.getByRole("heading", { name: "Test.Command.0" }),
  ).toBeVisible();
  await list
    .getByRole("option", { name: "选择包 1195: Test.Command.15" })
    .click({ button: "right" });
  const menu = page.getByRole("menu", { name: "包 #1195" });
  await expect(menu).toBeVisible();
  for (const group of ["查看", "复制", "筛选"])
    await expect(menu.getByRole("group", { name: group })).toBeVisible();
  await expect(
    menu.getByRole("menuitem", { name: "在右侧打开" }),
  ).toBeFocused();
  await expect(
    menu.getByRole("menuitem", { name: "复制命令" }),
  ).toHaveAttribute("aria-keyshortcuts", "Ctrl+C");
  await expect(
    page.getByRole("heading", { name: "Test.Command.0" }),
  ).toBeVisible();
  await page.keyboard.press("ArrowDown");
  await expect(menu.getByRole("menuitem", { name: "弹窗查看" })).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(menu).toHaveCount(0);
  await expect(list).toBeFocused();
  await expect(list).toHaveAttribute("aria-activedescendant", /-1195$/);
  await page.keyboard.press("Shift+F10");
  await expect(menu).toBeVisible();
  for (let i = 0; i < 3; i++) await page.keyboard.press("ArrowDown");
  await expect(menu.getByRole("menuitem", { name: "复制命令" })).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(menu).toHaveCount(0);
  await expect.poll(() => clipboard(page)).toBe("Test.Command.15");
  await expect(list).toBeFocused();
  await page.keyboard.press("ContextMenu");
  await expect(menu).toBeVisible();
  await page.keyboard.press("Enter");
  await expect(
    page.getByRole("heading", { name: "Test.Command.15" }),
  ).toBeVisible();
  // Near the bottom edge the menu opens upwards and stays inside the viewport.
  await list.evaluate((element) => {
    element.scrollTop = element.scrollHeight;
  });
  const last = list.getByRole("option", {
    name: "选择包 1200: Test.Command.0",
  });
  const row = (await last.boundingBox())!;
  await last.click({ button: "right", position: { x: 60, y: row.height - 4 } });
  const flipped = (await page
    .getByRole("menu", { name: "包 #1200" })
    .boundingBox())!;
  expect(flipped.y + flipped.height).toBeLessThanOrEqual(1000);
  expect(flipped.y).toBeLessThan(row.y);
  await page.mouse.click(1300, 760);
  await expect(page.getByRole("menu")).toHaveCount(0);
  await expect(
    page.getByRole("heading", { name: "Test.Command.15" }),
  ).toBeVisible();
});

test("checked rows get a batch menu, shift-click selects a range and control events only offer what applies", async ({
  page,
  request,
  context,
}) => {
  await grantClipboard(context);
  const id = await sessionId(request, "closed.qscap");
  await page.goto(`/?session=${id}`);
  const list = page.getByRole("listbox", { name: "数据包列表" });
  await expect(
    page.getByRole("heading", { name: "Test.Command.0" }),
  ).toBeVisible();
  await list
    .getByRole("option", { name: "选择包 1196: Test.Command.16" })
    .click({ modifiers: ["Control"] });
  await list
    .getByRole("option", { name: "选择包 1198: Test.Command.18" })
    .click({ modifiers: ["Control"] });
  await expect(page.getByRole("region", { name: "对比选择" })).toContainText(
    "已勾选 2",
  );
  await expect(
    page.getByRole("heading", { name: "Test.Command.0" }),
  ).toBeVisible();
  await list
    .getByRole("option", { name: "选择包 1198: Test.Command.18" })
    .click({ button: "right" });
  const batch = page.getByRole("menu", { name: "已选 2 个包" });
  await expect(
    batch.getByRole("menuitem", { name: "对比所选 2 个包" }),
  ).toBeVisible();
  await batch.getByRole("menuitem", { name: "复制所选命令" }).click();
  await expect
    .poll(() => clipboard(page))
    .toBe("Test.Command.16\nTest.Command.18");
  await list
    .getByRole("option", { name: "选择包 1200: Test.Command.0" })
    .click({ modifiers: ["Shift"] });
  await expect(page.getByRole("region", { name: "对比选择" })).toContainText(
    "已勾选 4",
  );
  await expect(
    page.getByRole("checkbox", { name: "对比选择 1199" }),
  ).toBeChecked();
  await list
    .getByRole("option", { name: "选择包 1197: Test.Command.17" })
    .click({ button: "right" });
  await expect(page.getByRole("menu", { name: "包 #1197" })).toBeVisible();
  await page.keyboard.press("Escape");
  await list
    .getByRole("option", { name: "选择包 1199: Test.Command.19" })
    .click({ button: "right" });
  await page
    .getByRole("menu", { name: "已选 4 个包" })
    .getByRole("menuitem", { name: "对比所选 4 个包" })
    .click();
  await expect(
    page.getByRole("dialog", { name: "多包对比 · 4 个包" }),
  ).toBeVisible();
  await page.keyboard.press("Escape");
  await page.getByRole("button", { name: "清空对比选择" }).click();
  await expect(page.getByRole("region", { name: "对比选择" })).toHaveCount(0);
  await page.getByRole("button", { name: "高级筛选", exact: true }).click();
  await page
    .getByRole("checkbox", {
      name: "显示采集事件（启动、丢包、结束）",
      exact: true,
    })
    .check();
  await page.getByRole("button", { name: "应用筛选", exact: true }).click();
  await page
    .getByRole("searchbox", { name: "搜索命令或序号" })
    .fill("controller_stopped");
  // The list follows the search box after a short pause and a filter change closes an open
  // menu, so the menu is opened only once the list shows the search result.
  await expect(
    list.getByRole("option").filter({ hasNotText: "controller_stopped" }),
  ).toHaveCount(0);
  await list
    .getByRole("option", { name: "选择包 1201: controller_stopped" })
    .click({ button: "right" });
  const control = page.getByRole("menu", { name: "采集事件 #1201" });
  await expect(
    control.getByRole("menuitem", { name: "复制事件名" }),
  ).toBeVisible();
  await expect(control.getByRole("menuitem")).not.toContainText(["加入对比"]);
  await expect(control.getByRole("menuitem", { name: "复制 SEQ" })).toHaveCount(
    0,
  );
  await control.getByRole("menuitem", { name: "隐藏采集事件" }).click();
  await expect(
    list.getByRole("option", { name: "选择包 1201: controller_stopped" }),
  ).toHaveCount(0);
  await page.locator(".toast").getByRole("button", { name: "撤销" }).click();
  await expect(
    list.getByRole("option", { name: "选择包 1201: controller_stopped" }),
  ).toBeVisible();
});

test("filters added from menus become removable chips and every change can be undone", async ({
  page,
  request,
}) => {
  const id = await sessionId(request, "closed.qscap");
  await page.goto(`/?session=${id}`);
  const list = page.getByRole("listbox", { name: "数据包列表" });
  const chips = page.getByRole("list", { name: "生效的筛选条件" });
  const toast = page.locator(".toast");
  const notice = page.getByText("不在筛选结果中", { exact: true });
  await list
    .getByRole("option", { name: "选择包 1195: Test.Command.15" })
    .click();
  await list
    .getByRole("option", { name: "选择包 1195: Test.Command.15" })
    .click({ button: "right" });
  await page.getByRole("menuitem", { name: "排除此命令" }).click();
  await expect(chips.getByRole("listitem")).toHaveText([
    "排除：Test.Command.15",
  ]);
  await expect(
    list.getByRole("option", { name: "选择包 1195: Test.Command.15" }),
  ).toHaveCount(0);
  await expect(
    page.getByRole("heading", { name: "Test.Command.15" }),
  ).toBeVisible();
  await expect(notice).toBeVisible();
  await expect(
    page.getByRole("button", { name: "在列表中定位" }),
  ).toBeDisabled();
  await expect(toast).toContainText("已排除 Test.Command.15");
  await toast.getByRole("button", { name: "撤销" }).click();
  await expect(chips).toHaveCount(0);
  await expect(notice).toHaveCount(0);
  await expect(
    list.getByRole("option", { name: "选择包 1195: Test.Command.15" }),
  ).toBeVisible();
  await list
    .getByRole("option", { name: "选择包 1195: Test.Command.15" })
    .click({ button: "right" });
  await page.getByRole("menuitem", { name: "排除此命令" }).click();
  await list
    .getByRole("option", { name: "选择包 1196: Test.Command.16" })
    .click({ button: "right" });
  await page.getByRole("menuitem", { name: "排除此命令" }).click();
  await direction(page, "↓ 收").click();
  await expect(chips.getByRole("listitem")).toHaveText([
    "只看接收",
    "排除：Test.Command.15",
    "排除：Test.Command.16",
  ]);
  await page
    .getByRole("button", { name: "移除筛选 排除：Test.Command.15" })
    .click();
  await expect(chips.getByRole("listitem")).toHaveText([
    "只看接收",
    "排除：Test.Command.16",
  ]);
  await expect(toast).toContainText("已移除筛选 排除：Test.Command.15");
  await toast.getByRole("button", { name: "撤销" }).click();
  await expect(chips.getByRole("listitem")).toHaveText([
    "只看接收",
    "排除：Test.Command.15",
    "排除：Test.Command.16",
  ]);
  await expect(
    page.getByRole("button", { name: "高级筛选", exact: true }),
  ).not.toContainText(/\d/);
  await page.getByRole("button", { name: "命令筛选", exact: true }).click();
  const commands = page.getByRole("dialog", { name: "命令筛选" });
  for (const n of [15, 16])
    await expect(
      commands.getByRole("button", {
        name: `排除 Test.Command.${n}`,
        exact: true,
      }),
    ).toHaveAttribute("aria-pressed", "true");
  await expect(
    commands.getByRole("button", { name: "排除 Test.Command.1", exact: true }),
  ).toHaveAttribute("aria-pressed", "false");
  await page.keyboard.press("Escape");
  await expect(commands).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "命令筛选", exact: true }),
  ).toBeFocused();
  await page.getByRole("button", { name: "清除全部筛选" }).click();
  await expect(chips).toHaveCount(0);
  await expect(direction(page, "全部")).toHaveAttribute("aria-pressed", "true");
  await toast.getByRole("button", { name: "撤销" }).click();
  await expect(chips.getByRole("listitem")).toHaveCount(3);
  await page.screenshot({
    path: resolve(root, "filter-chips.png"),
    fullPage: true,
  });
});

test("compare mode ticks rows on click and previews them without leaving the list", async ({
  page,
  request,
}) => {
  const id = await sessionId(request, "closed.qscap");
  await page.goto(`/?session=${id}`);
  const list = page.getByRole("listbox", { name: "数据包列表" });
  const basket = page.getByRole("region", { name: "对比选择" });
  const mode = page.getByRole("button", { name: "对比模式", exact: true });
  await expect(
    list.getByRole("option", { name: "选择包 1199: Test.Command.19" }),
  ).toBeVisible();
  await mode.click();
  await expect(mode).toHaveAttribute("aria-pressed", "true");
  await expect(list).toHaveAttribute("aria-multiselectable", "true");
  await expect(basket).toContainText("点击行勾选");
  await list
    .getByRole("option", { name: "选择包 1199: Test.Command.19" })
    .click();
  await list
    .getByRole("option", { name: "选择包 1198: Test.Command.18" })
    .click();
  await expect(basket).toContainText("已勾选 2");
  await expect(
    page.getByRole("heading", { name: "Test.Command.18" }),
  ).toBeVisible();
  await expect(
    page.getByRole("checkbox", { name: "对比选择 1199" }),
  ).toBeChecked();
  await list
    .getByRole("option", { name: "选择包 1199: Test.Command.19" })
    .click();
  await expect(
    page.getByRole("checkbox", { name: "对比选择 1199" }),
  ).not.toBeChecked();
  await expect(basket).toContainText("已勾选 1");
  await list
    .getByRole("option", { name: "选择包 1197: Test.Command.17" })
    .click();
  await page.getByRole("button", { name: "对比 2", exact: true }).click();
  await expect(
    page.getByRole("dialog", { name: "多包对比 · 2 个包" }),
  ).toBeVisible();
  await page.keyboard.press("Escape");
  // Leaving compare mode keeps the ticked packets; clicks open packets again.
  await mode.click();
  await expect(mode).toHaveAttribute("aria-pressed", "false");
  await list
    .getByRole("option", { name: "选择包 1196: Test.Command.16" })
    .click();
  await expect(basket).toContainText("已勾选 2");
  await expect(
    page.getByRole("checkbox", { name: "对比选择 1196" }),
  ).not.toBeChecked();
  await expect(
    page.getByRole("heading", { name: "Test.Command.16" }),
  ).toBeVisible();
});

test("the command filter offers commands seen in the record, picks exact names and takes typed rules", async ({
  page,
  request,
}) => {
  const id = await sessionId(request, "closed.qscap");
  await page.goto(`/?session=${id}`);
  const list = page.getByRole("listbox", { name: "数据包列表" });
  const chips = page.getByRole("list", { name: "生效的筛选条件" });
  await expect(list.getByRole("option").first()).toBeVisible();
  const trigger = page.getByRole("button", { name: "命令筛选", exact: true });
  await trigger.click();
  const popover = page.getByRole("dialog", { name: "命令筛选" });
  const options = popover
    .getByRole("list", { name: "本记录中的命令" })
    .getByRole("listitem");
  // Names sort with numbers in order; 长短 puts short names first.
  await expect(options).toHaveCount(24);
  await expect(options.first()).toContainText("Test.Command.0");
  await expect(options.first()).toContainText("59");
  await expect(options.nth(2)).toContainText("Test.Command.2");
  await expect(options.last()).toContainText("Test.TLV");
  await popover.getByRole("button", { name: "长短", exact: true }).click();
  await expect(options.first()).toContainText("Test.JCE");
  await expect(options.nth(1)).toContainText("Test.TLV");
  await expect(options.last()).toContainText("Test.SearchTarget");
  const input = popover.getByRole("textbox", { name: "查找命令或输入规则" });
  await input.fill("command.1");
  await expect(options).toHaveCount(11);
  // A picked command is exact: Test.Command.1 does not bring in Test.Command.10 to 19.
  await popover
    .getByRole("button", { name: "只看 Test.Command.1", exact: true })
    .click();
  await expect(chips.getByRole("listitem")).toHaveText([
    "只看：Test.Command.1",
  ]);
  await expect(page.locator(".list-summary")).toContainText("60 条");
  await expect(
    list.getByRole("option", { name: /Test\.Command\.1\d/ }),
  ).toHaveCount(0);
  await expect(trigger).toContainText("1");
  await expect(
    page.getByRole("button", { name: "高级筛选", exact: true }),
  ).not.toContainText(/\d/);
  // Typed text without wildcards matches inside names; a wildcard matches the whole name.
  await input.fill("Test.Command.1?");
  await expect(popover.getByText("* 和 ? 匹配完整命令名")).toBeVisible();
  await page.keyboard.press("Enter");
  await expect(input).toHaveValue("");
  await expect(chips.getByRole("listitem")).toHaveText([
    "只看：Test.Command.1",
    "只看：Test.Command.1?",
  ]);
  await expect(
    popover.getByRole("button", { name: "移除规则 Test.Command.1?" }),
  ).toBeVisible();
  await expect(
    list.getByRole("option", { name: /Test\.Command\.1\d$/ }).first(),
  ).toBeVisible();
  await expect(
    list.getByRole("option", { name: /Test\.Command\.[02-9]$/ }),
  ).toHaveCount(0);
  await input.fill("Search");
  await popover.getByRole("button", { name: "排除规则 Search" }).click();
  await expect(chips.getByRole("listitem")).toHaveText([
    "只看：Test.Command.1",
    "只看：Test.Command.1?",
    "排除：含 Search",
  ]);
  await popover
    .getByRole("button", { name: "移除规则 Test.Command.1?" })
    .click();
  await expect(chips.getByRole("listitem")).toHaveText([
    "只看：Test.Command.1",
    "排除：含 Search",
  ]);
  await popover.getByRole("button", { name: "清除命令筛选" }).click();
  await expect(chips).toHaveCount(0);
  await expect(trigger).not.toContainText(/\d/);
  // Opening the command filter closes the advanced panel, and an outside click closes it.
  await page.getByRole("button", { name: "高级筛选", exact: true }).click();
  await expect(popover).toHaveCount(0);
  await expect(page.getByRole("form", { name: "高级筛选" })).toBeVisible();
  await trigger.click();
  await expect(page.getByRole("form", { name: "高级筛选" })).toHaveCount(0);
  await expect(popover).toBeVisible();
  await page.locator(".bytes-caption").click();
  await expect(popover).toHaveCount(0);
});

test("the detail header spans both columns, names formats in full and copies labelled facts", async ({
  page,
  request,
  context,
}) => {
  await grantClipboard(context);
  const id = await sessionId(request, "closed.qscap");
  await page.goto(`/?session=${id}`);
  const detail = page.getByRole("region", { name: "包详情" });
  await expect(
    detail.getByRole("heading", { name: "Test.Command.0" }),
  ).toBeVisible();
  await expect(
    detail.getByRole("tablist", { name: "查看格式" }).getByRole("tab"),
  ).toHaveText(["Protobuf", "TLV16-BE", "JCE / Tars", "HTTP", "Hex"]);
  await expect(detail.getByRole("tab", { name: "Protobuf" })).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await detail.getByRole("button", { name: "复制 SEQ" }).click();
  await expect
    .poll(() => page.evaluate(() => navigator.clipboard.readText()))
    .toBe("9007199254742193");
  await detail.getByRole("button", { name: "复制 SHA256" }).click();
  await expect
    .poll(() => page.evaluate(() => navigator.clipboard.readText()))
    .toMatch(/^[0-9a-f]{64}$/);
  // Identity and facts sit above both the tree and the raw bytes; the two tool rows line up.
  const head = (await detail.locator(".detail-head").boundingBox())!;
  const bytes = (await page
    .getByRole("region", { name: "原始字节面板" })
    .boundingBox())!;
  expect(bytes.y).toBeGreaterThanOrEqual(head.y + head.height - 1);
  // The format row lines up with the raw bytes caption; tree tools keep their text labels.
  for (const [name, text] of [
    ["全部展开", "展开"],
    ["全部折叠", "折叠"],
    ["解析范围", "范围"],
  ])
    await expect(detail.getByRole("button", { name, exact: true })).toHaveText(
      text,
    );
  const tools = (await detail.locator(".tools-view").boundingBox())!;
  const caption = (await detail.locator(".bytes-caption").boundingBox())!;
  expect(Math.abs(tools.y - caption.y)).toBeLessThan(1);
  expect(Math.abs(tools.height - caption.height)).toBeLessThan(1);
  await page.evaluate(() => document.fonts.ready);
  await page.screenshot({
    path: resolve(root, "detail-header.png"),
    fullPage: true,
  });
});

test("field tree menu explains disabled items, skips them by keyboard and flips at the right edge", async ({
  page,
  request,
  context,
}) => {
  await grantClipboard(context);
  const id = await sessionId(request, "closed.qscap");
  await page.goto(`/?session=${id}`);
  await page.getByRole("button", { name: "折叠原始字节" }).click();
  const tree = page.getByRole("tree", { name: "解码字段树" });
  const field = tree.locator('.field-row[data-path="2"]');
  await expect(field).toBeVisible();
  const box = (await field.boundingBox())!;
  await field.click({
    button: "right",
    position: { x: box.width - 6, y: box.height / 2 },
  });
  const menu = page.getByRole("menu", { name: "字段 2" });
  const placed = (await menu.boundingBox())!;
  expect(placed.x + placed.width).toBeLessThanOrEqual(1600);
  expect(placed.x).toBeLessThan(box.x + box.width - 6);
  expect(Math.abs(placed.y - (box.y + box.height / 2))).toBeLessThan(4);
  const expand = menu.getByRole("menuitem", { name: "展开子树" });
  await expect(expand).toHaveAttribute("aria-disabled", "true");
  await expect(expand).toContainText("没有子字段");
  await expand.click({ force: true });
  await expect(menu).toBeVisible();
  await expect(
    menu.getByRole("menuitem", { name: "在 Hex 中定位" }),
  ).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("ArrowDown");
  await expect(menu.getByRole("menuitem", { name: "复制值" })).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("Enter");
  await expect.poll(() => clipboard(page)).toBe("2");
  const label = field.locator(".json-key");
  await expect(label).toBeFocused();
  await page.keyboard.press("Shift+F10");
  await expect(menu).toBeVisible();
  await page.keyboard.press("End");
  await page.keyboard.press("Enter");
  await expect(page.getByRole("button", { name: "解析范围" })).toContainText(
    "[5, 7)",
  );
});

test("closing tasks for an update saves running recordings through the local control entry", async ({
  request,
}) => {
  const headers = { "X-QScope-Client": "web-v1" };
  const id = await sessionId(request, "live.qscap");
  const started = await (
    await request.post("/api/v1/recordings", {
      headers,
      data: { session_id: id },
    })
  ).json();
  expect(started.state).toBe("recording");
  const closed = await request.post("/api/v1/service/control", {
    headers,
    data: { action: "close_tasks", client_id: "0".repeat(48) },
  });
  expect(closed.ok()).toBeTruthy();
  expect(await closed.json()).toMatchObject({ recordings: 1, qq: [] });
  const jobs = await (
    await request.get("/api/v1/recordings", { headers })
  ).json();
  expect(jobs.find((job: { id: string }) => job.id === started.id).state).toBe(
    "stopped",
  );
});

test("an update names the research QQ and recordings it closes after the package is verified", async ({
  page,
}) => {
  let stage = "available";
  const status = () => ({
    current: "0.1.1",
    install: "installer",
    unsupported: null,
    stage,
    latest: "0.1.2",
    notes_url: "https://github.com/sj817/QScope/releases/tag/v0.1.2",
    downloaded: "0",
    total: "0",
    error: null,
    checked_unix: null,
    check_on_launch: true,
    proxy: null,
  });
  for (const path of ["**/api/v1/update", "**/api/v1/update/check"])
    await page.route(path, (route) => route.fulfill({ json: status() }));
  let installs = 0;
  await page.route("**/api/v1/update/install", (route) => {
    installs++;
    stage = "closing";
    return route.fulfill({ json: status() });
  });
  await page.route("**/api/v1/settings/qq", (route) =>
    route.fulfill({
      json: {
        build: "9.9.37-54395",
        stage: "running",
        installed: true,
        available: true,
        message: "QQ 已运行，流量正在记录",
        downloaded: "0",
        total: "334442672",
        pid: 6789,
        session_id: null,
        error: null,
      },
    }),
  );
  await page.route("**/api/v1/recordings", (route) =>
    route.fulfill({
      json: [
        {
          id: "0123456789abcdef01234567",
          source_session: "fedcba9876543210fedcba98",
          state: "recording",
          path: "synthetic.qscap",
          started_filetime: "134000000000000000",
          packets: "3",
          bytes: "512",
          source_first_event: null,
          source_last_event: null,
          error: null,
        },
      ],
    }),
  );
  await page.goto("/?page=settings&section=update");
  const settings = page.getByRole("dialog", { name: "QScope 设置" });
  await expect(settings).toContainText(
    "更新前会关闭研究 QQ（PID 6789）和 1 段录制",
  );
  await settings
    .getByRole("button", { name: "关闭 QQ 并更新到 0.1.2", exact: true })
    .click();
  await expect(settings).toContainText(
    "安装包已校验，正在保存录制并关闭研究 QQ",
  );
  expect(installs).toBe(1);
});

test("settings check the runtime with a fix for each part and keep update proxies to plain http", async ({
  page,
}) => {
  await page.goto("/?page=settings");
  const settings = page.getByRole("dialog", { name: "QScope 设置" });
  // The synthetic workspace has neither a built collector nor a QQ download, so the check opens by itself.
  const runtime = settings.getByLabel("运行环境");
  if (
    (process.platform === "win32" && process.arch === "x64") ||
    (process.platform === "darwin" && process.arch === "arm64")
  ) {
    await expect(runtime).toContainText("2 项需要处理");
    await expect(runtime.locator(".runtime-list li")).toHaveCount(4);
    await expect(runtime.locator("li.bad")).toHaveCount(2);
    await expect(runtime).toContainText(
      process.platform === "darwin"
        ? "node tools/build-native-macos.ts"
        : "node tools/build-native.ts",
    );
  } else {
    await expect(runtime).toContainText("2 项需要处理");
    await expect(runtime.locator(".runtime-list li")).toHaveCount(3);
    await expect(runtime.locator("li.bad")).toHaveCount(2);
    await expect(runtime).toContainText("可读取与分析抓包文件");
    await expect(runtime).not.toContainText("node tools/build-native.ts");
    await expect(
      settings.getByRole("button", { name: "此平台暂未开放采集" }),
    ).toBeDisabled();
    await expect(
      settings.getByRole("button", { name: "下载并启动 QQ", exact: true }),
    ).toHaveCount(0);
    await expect(settings).not.toContainText("54395");
    await expect(settings).not.toContainText("334442672");
  }
  await expect(runtime.getByRole("button", { name: "一键修复" })).toHaveCount(
    0,
  );
  await settings.getByRole("tab", { name: /更新/ }).click();
  await expect(page).toHaveURL(/section=update/);
  await expect(settings).toContainText(
    process.platform === "win32" && process.arch === "x64"
      ? "开发构建不支持在线更新"
      : "在线更新目前只支持 Windows x64",
  );
  await expect(
    settings.getByRole("button", { name: "检查更新" }),
  ).toBeDisabled();
  const proxy = settings.getByLabel("更新代理");
  await proxy.fill("socks5://127.0.0.1:1080");
  await settings.getByRole("button", { name: "保存", exact: true }).click();
  await expect(settings.getByRole("alert")).toContainText("只支持 http://");
  await proxy.fill("http://127.0.0.1:7890/");
  await settings.getByRole("button", { name: "保存", exact: true }).click();
  await expect(
    page.getByText("更新代理已保存：http://127.0.0.1:7890"),
  ).toBeVisible();
  await expect(proxy).toHaveValue("http://127.0.0.1:7890");
  await proxy.fill("");
  await settings.getByRole("button", { name: "保存", exact: true }).click();
  await expect(page.getByText("更新将直接连接 GitHub")).toBeVisible();
});
