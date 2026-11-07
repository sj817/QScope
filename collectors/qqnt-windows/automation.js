(() => {
  const load =
    typeof require === "function"
      ? require
      : process.mainModule?.require.bind(process.mainModule);
  if (!load)
    throw new Error(
      "QScope automation requires the main-process module loader",
    );
  const fs = load("node:fs");
  const statusPath = process.env.QSCOPE_STATE_ROOT + "/.qscope/devtools.json";
  const identity = globalThis.__qscopeProcessIdentity;
  const port = Number(process.env.QSCOPE_DEVTOOLS_PORT);
  const status = {
    schema_version: 1,
    ...identity,
    endpoint: `http://127.0.0.1:${port}`,
  };

  try {
    if (
      process.type !== "browser" ||
      !identity ||
      identity.pid !== process.pid
    ) {
      throw new Error(
        "QScope automation requires its owned Electron main process",
      );
    }
    if (!Number.isInteger(port) || port < 1024 || port > 65535) {
      throw new Error("Invalid QScope DevTools port");
    }
    const { app, BrowserWindow } = load("electron");
    const http = load("node:http");
    const token = load("node:crypto").randomBytes(32).toString("hex");
    const windowFor = (id) => {
      const window = BrowserWindow.fromId(id);
      if (!window || window.isDestroyed() || window.webContents.isDestroyed()) {
        throw new Error("Window no longer exists. Refresh the window list");
      }
      return window;
    };
    const bounded = async (operation) => {
      let timer;
      try {
        return await Promise.race([
          operation,
          new Promise((_, reject) => {
            timer = setTimeout(
              () => reject(new Error("Renderer operation timed out")),
              8000,
            );
            timer.unref?.();
          }),
        ]);
      } finally {
        clearTimeout(timer);
      }
    };
    const execute = (contents, expression) =>
      bounded(contents.executeJavaScript(expression, true));

    function snapshotDocument() {
      const visible = (element) => {
        const rect = element.getBoundingClientRect();
        const style = getComputedStyle(element);
        return (
          rect.width > 0 &&
          rect.height > 0 &&
          style.visibility !== "hidden" &&
          style.display !== "none"
        );
      };
      const selectorFor = (element) => {
        if (element.id) {
          const selector = "#" + CSS.escape(element.id);
          if (document.querySelectorAll(selector).length === 1) return selector;
        }
        const parts = [];
        let current = element;
        while (
          current &&
          current !== document.documentElement &&
          parts.length < 12
        ) {
          const tag = current.localName;
          const siblings = [...current.parentElement.children].filter(
            (child) => child.localName === tag,
          );
          parts.unshift(`${tag}:nth-of-type(${siblings.indexOf(current) + 1})`);
          const selector = parts.join(" > ");
          if (document.querySelectorAll(selector).length === 1) return selector;
          current = current.parentElement;
        }
        return null;
      };
      const elements = [];
      let limited = false;
      for (const element of document.querySelectorAll(
        'button,a,input,textarea,select,[role="button"],[role="tab"],[role="textbox"],[contenteditable="true"],[tabindex]',
      )) {
        if (!visible(element)) continue;
        if (elements.length >= 300) {
          limited = true;
          break;
        }
        elements.push({
          selector: selectorFor(element),
          tag: element.localName,
          role: element.getAttribute("role"),
          name: (
            element.getAttribute("aria-label") ||
            element.getAttribute("placeholder") ||
            element.innerText ||
            element.title ||
            ""
          ).slice(0, 300),
          type: element.getAttribute("type"),
          disabled:
            Boolean(element.disabled) ||
            element.getAttribute("aria-disabled") === "true",
        });
      }
      const text = document.body?.innerText || "";
      return {
        title: document.title,
        url: location.href,
        text: text.slice(0, 20000),
        elements,
        limited: limited || text.length > 20000,
      };
    }

    function locateElement(selector, editing) {
      const matches = [...document.querySelectorAll(selector)].filter(
        (element) => {
          const rect = element.getBoundingClientRect();
          const style = getComputedStyle(element);
          return (
            rect.width > 0 &&
            rect.height > 0 &&
            style.visibility !== "hidden" &&
            style.display !== "none"
          );
        },
      );
      if (matches.length !== 1)
        throw new Error(
          `Expected one visible element, found ${matches.length}`,
        );
      const element = matches[0];
      if (element.disabled || element.getAttribute("aria-disabled") === "true")
        throw new Error("Element is disabled");
      if (
        editing &&
        !element.isContentEditable &&
        !["INPUT", "TEXTAREA"].includes(element.tagName)
      ) {
        throw new Error("Element is not editable");
      }
      if (editing && element.readOnly) throw new Error("Element is read-only");
      element.scrollIntoView({ block: "center", inline: "center" });
      element.focus();
      if (editing) {
        if (element.isContentEditable) {
          const range = document.createRange();
          range.selectNodeContents(element);
          const selection = getSelection();
          selection.removeAllRanges();
          selection.addRange(range);
        } else {
          element.select();
        }
      }
      const rect = element.getBoundingClientRect();
      const x = Math.round(rect.left + rect.width / 2);
      const y = Math.round(rect.top + rect.height / 2);
      const hit = document.elementFromPoint(x, y);
      if (!hit || !element.contains(hit))
        throw new Error("Element is covered or outside the viewport");
      return { x, y };
    }

    const bridge = {
      info() {
        return {
          version: 1,
          ...identity,
          build: process.env.QSCOPE_CLIENT_BUILD,
          endpoint: status.endpoint,
        };
      },
      async run(request) {
        if (!request || typeof request.action !== "string")
          throw new Error("Invalid automation request");
        if (!["windows", "snapshot", "screenshot"].includes(request.action)) {
          if (
            request.identity?.pid !== identity.pid ||
            request.identity?.birth_filetime !== identity.birth_filetime
          ) {
            throw new Error(
              "QQ process identity changed. Refresh the selected window",
            );
          }
        }
        await app.whenReady();
        if (request.action === "windows") {
          return BrowserWindow.getAllWindows()
            .filter((window) => !window.isDestroyed())
            .map((window) => ({
              id: window.id,
              title: window.getTitle().slice(0, 300),
              visible: window.isVisible(),
              web_contents_id: window.webContents.id,
              url: window.webContents.getURL().slice(0, 2048),
              devtools_open: window.webContents.isDevToolsOpened(),
              focused: window.isFocused(),
              width: window.getBounds().width,
              height: window.getBounds().height,
            }));
        }
        if (request.action === "main-eval") {
          if (
            typeof request.expression !== "string" ||
            request.expression.length > 32768
          )
            throw new Error("Expression exceeds 32 KiB");
          const result = load("node:vm").runInNewContext(
            request.expression,
            {
              require: load,
              process,
              Buffer,
              console,
              setTimeout,
              clearTimeout,
            },
            { timeout: 5000 },
          );
          return bounded(Promise.resolve(result));
        }
        if (!Number.isInteger(request.window_id))
          throw new Error("Choose an explicit window ID");
        const window = windowFor(request.window_id);
        const contents = window.webContents;
        switch (request.action) {
          case "snapshot":
            return execute(contents, `(${snapshotDocument.toString()})()`);
          case "eval":
            if (
              typeof request.expression !== "string" ||
              request.expression.length > 32768
            )
              throw new Error("Expression exceeds 32 KiB");
            return execute(contents, request.expression);
          case "click": {
            if (
              typeof request.selector !== "string" ||
              request.selector.length > 4096
            )
              throw new Error("Invalid CSS selector");
            window.focus();
            const point = await execute(
              contents,
              `(${locateElement.toString()})(${JSON.stringify(request.selector)},false)`,
            );
            contents.sendInputEvent({
              type: "mouseDown",
              ...point,
              button: "left",
              clickCount: 1,
            });
            contents.sendInputEvent({
              type: "mouseUp",
              ...point,
              button: "left",
              clickCount: 1,
            });
            return { clicked: true, window_id: window.id };
          }
          case "fill":
            if (
              typeof request.text !== "string" ||
              request.text.length > 16384 ||
              typeof request.selector !== "string" ||
              request.selector.length > 4096
            ) {
              throw new Error("Invalid selector or text exceeding 16 KiB");
            }
            window.focus();
            await execute(
              contents,
              `(${locateElement.toString()})(${JSON.stringify(request.selector)},true)`,
            );
            if (request.text) await bounded(contents.insertText(request.text));
            else {
              contents.sendInputEvent({
                type: "keyDown",
                keyCode: "Backspace",
              });
              contents.sendInputEvent({ type: "keyUp", keyCode: "Backspace" });
            }
            return {
              filled: true,
              window_id: window.id,
              characters: request.text.length,
            };
          case "press":
            if (typeof request.key !== "string" || request.key.length > 40)
              throw new Error("Invalid key");
            window.focus();
            contents.sendInputEvent({ type: "keyDown", keyCode: request.key });
            contents.sendInputEvent({ type: "keyUp", keyCode: request.key });
            return { pressed: true, window_id: window.id };
          case "inspect": {
            const frontend = new URL(request.frontend_url);
            if (
              frontend.protocol !== "http:" ||
              frontend.hostname !== "127.0.0.1" ||
              !frontend.port ||
              frontend.username ||
              frontend.password ||
              frontend.search ||
              frontend.hash ||
              !/^\/[a-f0-9]{48}\/$/.test(frontend.pathname)
            )
              throw new Error("Invalid local inspector endpoint");
            if (
              typeof request.client_path !== "string" ||
              !/[\\/]chii[\\/]public[\\/]target\.js$/.test(request.client_path)
            ) {
              throw new Error("Choose the pinned Chii target script");
            }
            const metadata = fs.statSync(request.client_path);
            if (!metadata.isFile() || metadata.size > 2 * 1024 * 1024)
              throw new Error("Invalid inspector script size");
            const script = fs.readFileSync(request.client_path);
            if (
              load("node:crypto")
                .createHash("sha256")
                .update(script)
                .digest("hex") !== request.client_sha256
            ) {
              throw new Error("Inspector script hash mismatch");
            }
            return execute(
              contents,
              `(() => {
              const server = ${JSON.stringify(frontend.href)};
              if (window.__qscopeChii) {
                if (window.__qscopeChii.server !== server) throw new Error('Refresh this QQ window before changing inspector services');
                return { target_id: window.__qscopeChii.id };
              }
              window.ChiiServerUrl = server;
              window.ChiiTitle = ${JSON.stringify("QScope · " + identity.pid + " · " + window.id)};
              ${script.toString("utf8")}
              const id = sessionStorage.getItem('chii-id');
              if (!id) throw new Error('Inspector did not register its target');
              window.__qscopeChii = { server, id };
              return { target_id: id };
            })()`,
            );
          }
          case "screenshot": {
            const image = await bounded(contents.capturePage());
            const bytes = image.toPNG();
            if (bytes.length > 8 * 1024 * 1024)
              throw new Error("Screenshot exceeds 8 MiB");
            return {
              png_base64: bytes.toString("base64"),
              window_id: window.id,
              ...image.getSize(),
            };
          }
          default:
            throw new Error("Unknown QScope automation action");
        }
      },
    };
    Object.defineProperty(globalThis, "__qscopeAutomation", {
      value: Object.freeze(bridge),
    });
    const saveStatus = (value) => {
      const temporary = statusPath + "." + process.pid + ".writing";
      fs.writeFileSync(temporary, JSON.stringify(value), { mode: 0o600 });
      fs.renameSync(temporary, statusPath);
    };
    const respond = (response, code, value) => {
      const body = JSON.stringify(value, (_, value) =>
        typeof value === "bigint" ? value.toString() : value,
      );
      if (Buffer.byteLength(body) > 12 * 1024 * 1024)
        throw new Error("Automation response exceeds 12 MiB");
      response.writeHead(code, {
        "Content-Type": "application/json",
        "Cache-Control": "no-store",
        "X-Content-Type-Options": "nosniff",
      });
      response.end(body);
    };
    const server = http.createServer(async (request, response) => {
      if (
        request.headers.host !== `127.0.0.1:${port}` ||
        request.headers.origin ||
        request.headers["x-qscope-client"] !== "web-v1" ||
        request.headers.authorization !== `Bearer ${token}`
      ) {
        respond(response, 403, {
          code: "forbidden",
          message: "Research bridge credentials required",
        });
        return;
      }
      try {
        if (request.method === "GET" && request.url === "/qscope/v1/info") {
          respond(response, 200, bridge.info());
          return;
        }
        if (
          request.method !== "POST" ||
          request.url !== "/qscope/v1/action" ||
          request.headers["content-type"]?.split(";")[0] !== "application/json"
        ) {
          respond(response, 404, {
            code: "not_found",
            message: "Unknown bridge operation",
          });
          return;
        }
        const chunks = [];
        let size = 0;
        for await (const chunk of request) {
          size += chunk.length;
          if (size > 64 * 1024)
            throw new Error("Automation request exceeds 64 KiB");
          chunks.push(chunk);
        }
        const result = await bridge.run(
          JSON.parse(Buffer.concat(chunks).toString("utf8")),
        );
        respond(response, 200, result ?? null);
      } catch (error) {
        respond(response, 400, {
          code: "automation",
          message: String(error.message).slice(0, 2000),
        });
      }
    });
    server.maxConnections = 8;
    server.requestTimeout = 10000;
    server.headersTimeout = 5000;
    server.keepAliveTimeout = 1000;
    server.on("error", (error) => {
      try {
        saveStatus({ ...status, ready: false, error: error.message });
      } catch {
        /* 状态报告允许失败 */
      }
    });
    server.listen(port, "127.0.0.1", () =>
      saveStatus({ ...status, token, ready: true }),
    );
  } catch (error) {
    try {
      fs.writeFileSync(
        statusPath,
        JSON.stringify({
          ...status,
          ready: false,
          error: String(error.message).slice(0, 2000),
        }),
      );
    } catch {
      /* 状态报告允许失败 */
    }
  }
})();
