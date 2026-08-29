    const boardTabs = ["global", "network", "services"];
    const boardNetworkTabs = ["all", "general", "trade", "search", "request"];
    const boardPageSize = 50;
    let activeBoardTab = "network";
    let activeBoardNetworkTab = "all";
    let boardPayload = null;
    let boardSearchQuery = "";
    let boardSearchTimer = null;
    let boardLoadInFlight = false;
    const boardSourceState = {
      network: { loaded: false, cursors: {}, hasMore: true },
      global: { loaded: false, cursor: null, hasMore: true },
      services: { loaded: false, cursor: null, hasMore: true },
    };

    function boardSetStatus(message, isError = false) {
      const status = qs("board-status");
      if (!status) return;
      status.textContent = message;
      status.className = isError ? "status-text error" : "status-text";
    }

    function boardMessageContent(message) {
      let content = message?.content;
      if (message?.source === "service" && content?.message !== undefined) {
        content = content.message;
      }
      if (typeof content === "string") return content;
      if (content?.text) return String(content.text);
      if (content == null) return "";
      return JSON.stringify(content, null, 2);
    }

    function boardFilteredMessages() {
      const messages = safeArray(boardPayload?.messages);
      if (activeBoardTab === "global") return messages.filter((message) => message.source === "global");
      if (activeBoardTab === "services") return messages.filter((message) => message.source === "service");
      if (activeBoardNetworkTab === "all") {
        return messages.filter((message) => message.source === "network");
      }
      return messages.filter((message) => (
        message.source === "network" && message.category === activeBoardNetworkTab
      ));
    }

    function boardCountForSource(source) {
      return safeArray(boardPayload?.messages).filter((message) => message.source === source).length;
    }

    function boardSetTabCount(tab, count) {
      const element = document.querySelector(`[data-board-count="${tab}"]`);
      if (element) element.textContent = count ? `(${count})` : "";
    }

    function renderBoardMessages() {
      const list = qs("board-messages");
      if (!list) return;
      const messages = boardFilteredMessages();
      if (!messages.length) {
        list.innerHTML = empty(boardSearchQuery ? "No matching messages." : "No messages in this Board view.");
        return;
      }
      list.innerHTML = messages.map((message) => {
        const author = message.source === "global"
          ? compactId(message.author_node_id || "Network", 24)
          : (message.source === "service"
            ? valueOrDash(message.service_name)
            : valueOrDash(message.author_display_name || message.author_public_id || message.author_node_id));
        const showCategory = activeBoardTab === "network" && activeBoardNetworkTab === "all";
        const category = String(message.category || "").trim();
        const categoryLabel = category
          ? category.charAt(0).toUpperCase() + category.slice(1)
          : "";
        const categoryMarkup = showCategory && categoryLabel
          ? `<span class="board-message-category">[${escapeHtml(categoryLabel)}]</span>`
          : "";
        return `
          <article class="board-message">
            <div class="board-message-head">
              ${categoryMarkup}
              <span class="board-message-time">${escapeHtml(formatTime(message.created_at))}</span>
            </div>
            <div class="board-message-content">${escapeHtml(boardMessageContent(message))}</div>
            <div class="board-message-foot">
              <span class="board-message-author">${escapeHtml(author)}</span>
            </div>
          </article>`;
      }).join("");
    }

    function syncBoardTabs() {
      document.querySelectorAll("[data-board-tab]").forEach((item) => {
        const selected = item.dataset.boardTab === activeBoardTab;
        item.classList.toggle("active", selected);
        item.setAttribute("aria-selected", String(selected));
      });
      document.querySelectorAll("[data-board-network-tab]").forEach((item) => {
        const selected = item.dataset.boardNetworkTab === activeBoardNetworkTab;
        item.classList.toggle("active", selected);
        item.setAttribute("aria-selected", String(selected));
      });
      const networkTabs = qs("board-network-tabs");
      if (networkTabs) networkTabs.hidden = activeBoardTab !== "network";
      boardSetTabCount("global", boardCountForSource("global"));
      boardSetTabCount("network", boardCountForSource("network"));
      boardSetTabCount("services", boardCountForSource("service"));
    }

    function renderBoard() {
      syncBoardTabs();
      renderBoardMessages();
      const globalError = boardPayload?.global_error;
      const serviceError = boardPayload?.service_error;
      const suffix = [globalError ? "Global data unavailable" : "", serviceError ? "ServiceNet unavailable" : ""]
        .filter(Boolean)
        .join(" | ");
      const searchSuffix = boardSearchQuery ? ` for "${boardSearchQuery}"` : "";
      boardSetStatus(
        `Showing ${boardFilteredMessages().length} messages${searchSuffix}${suffix ? ` | ${suffix}` : ""}`,
        Boolean((globalError && activeBoardTab === "global") || (serviceError && activeBoardTab === "services")),
      );
    }

    function boardResetSourceState(source, resetAll = false) {
      if (source === "network" && !resetAll && activeBoardNetworkTab !== "all") {
        delete boardSourceState.network.cursors[activeBoardNetworkTab];
        boardSourceState.network.hasMore = true;
        return;
      }
      if (source === "network") boardSourceState.network = { loaded: false, cursors: {}, hasMore: true };
      if (source === "global") boardSourceState.global = { loaded: false, cursor: null, hasMore: true };
      if (source === "services") boardSourceState.services = { loaded: false, cursor: null, hasMore: true };
    }

    function boardRemoveSourceMessages(source, category = null) {
      const sourceName = source === "services" ? "service" : source;
      boardPayload.messages = safeArray(boardPayload.messages)
        .filter((message) => (
          message.source !== sourceName
          || (source === "network" && category && message.category !== category)
        ));
    }

    function boardApplyPayload(payload, source) {
      if (!boardPayload || boardSearchQuery) {
        boardPayload = { ...payload, messages: safeArray(payload.messages) };
        return;
      }
      const category = source === "network" && activeBoardNetworkTab !== "all"
        ? activeBoardNetworkTab
        : null;
      boardRemoveSourceMessages(source, category);
      const existing = new Map();
      safeArray(boardPayload.messages).forEach((message) => {
        existing.set(`${message.source}:${message.message_id}`, message);
      });
      safeArray(payload.messages).forEach((message) => {
        existing.set(`${message.source}:${message.message_id}`, message);
      });
      boardPayload.messages = Array.from(existing.values()).sort((left, right) => (
        Number(right.created_at || 0) - Number(left.created_at || 0)
      ));
      if (payload.channels) boardPayload.channels = payload.channels;
      if (payload.service_agents) boardPayload.service_agents = payload.service_agents;
      if (Object.prototype.hasOwnProperty.call(payload, "global_error")) boardPayload.global_error = payload.global_error;
      if (Object.prototype.hasOwnProperty.call(payload, "service_error")) boardPayload.service_error = payload.service_error;
    }

    function boardUpdateSourceState(payload, source) {
      const next = payload?.next || {};
      if (source === "network") {
        const cursors = next.network || {};
        Object.keys(cursors).forEach((category) => {
          boardSourceState.network.cursors[category] = cursors[category];
        });
        boardSourceState.network.hasMore = Object.values(boardSourceState.network.cursors)
          .some((cursor) => cursor?.has_more);
        boardSourceState.network.loaded = true;
      }
      if (source === "global") {
        boardSourceState.global.cursor = next.global || null;
        boardSourceState.global.hasMore = Boolean(next.global?.has_more);
        boardSourceState.global.loaded = true;
      }
      if (source === "services") {
        boardSourceState.services.cursor = next.services || null;
        boardSourceState.services.hasMore = Boolean(next.services?.has_more);
        boardSourceState.services.loaded = true;
      }
    }

    function boardSourceQuery(source, options = {}) {
      const params = new URLSearchParams({
        source,
        limit: String(boardPageSize),
      });
      if (source === "network") {
        if (activeBoardNetworkTab !== "all") params.set("category", activeBoardNetworkTab);
        if (Object.keys(boardSourceState.network.cursors).length) {
          params.set("network_cursors", JSON.stringify(boardSourceState.network.cursors));
        }
      }
      if (source === "global" && boardSourceState.global.cursor?.before_sequence != null) {
        params.set("global_before_sequence", String(boardSourceState.global.cursor.before_sequence));
      }
      if (source === "services" && boardSourceState.services.cursor) {
        const cursor = boardSourceState.services.cursor;
        if (cursor.before_created_at != null) {
          params.set("before_created_at", String(cursor.before_created_at));
        }
        if (cursor.before_message_id) {
          params.set("before_message_id", cursor.before_message_id);
        }
      }
      if (options.search) params.set("search", options.search);
      return `/v1/client/board?${params.toString()}`;
    }

    function boardSourceHasMore(source) {
      if (source === "network" && activeBoardNetworkTab !== "all") {
        return Boolean(boardSourceState.network.cursors[activeBoardNetworkTab]?.has_more);
      }
      return boardSourceState[source].hasMore;
    }

    async function loadBoardSource(source, options = {}) {
      if (boardLoadInFlight) return false;
      if (!options.reset && boardSourceState[source].loaded && !boardSourceHasMore(source)) return true;
      if (options.reset) boardResetSourceState(source);
      boardLoadInFlight = true;
      try {
        if (!options.silent) boardSetStatus("Loading Message Board...");
        const payload = await fetchJson(boardSourceQuery(source), { auth: true });
        boardApplyPayload(payload, source);
        boardUpdateSourceState(payload, source);
        renderBoard();
        return true;
      } catch (error) {
        boardSetStatus(error.message, true);
        return false;
      } finally {
        boardLoadInFlight = false;
      }
    }

    async function loadBoardSearch(search) {
      const normalized = String(search || "").trim();
      if (!normalized) {
        boardSearchQuery = "";
        boardPayload = null;
        boardResetSourceState("network", true);
        boardResetSourceState("global");
        boardResetSourceState("services");
        return loadBoardSource("network", { reset: true });
      }
      if (boardLoadInFlight) return false;
      boardLoadInFlight = true;
      boardSearchQuery = normalized;
      try {
        boardSetStatus("Searching Message Board...");
        boardPayload = await fetchJson(boardSourceQuery("network", { search: normalized }), { auth: true });
        renderBoard();
        return true;
      } catch (error) {
        boardSetStatus(error.message, true);
        return false;
      } finally {
        boardLoadInFlight = false;
      }
    }

    async function loadBoardMessages(options = {}) {
      if (!qs("board-messages")) return false;
      const search = qs("board-search")?.value || "";
      if (search.trim()) return loadBoardSearch(search);
      boardSearchQuery = "";
      const source = activeBoardTab === "global"
        ? "global"
        : activeBoardTab === "services" ? "services" : "network";
      return loadBoardSource(source, { reset: true, silent: options.silent });
    }

    async function loadBoardMore() {
      if (boardSearchQuery) return false;
      const source = activeBoardTab === "global"
        ? "global"
        : activeBoardTab === "services" ? "services" : "network";
      return loadBoardSource(source);
    }

    function bindBoardControls() {
      document.querySelectorAll("[data-board-tab]").forEach((button) => {
        button.addEventListener("click", () => {
          activeBoardTab = boardTabs.includes(button.dataset.boardTab) ? button.dataset.boardTab : "network";
          renderBoard();
          if (!boardSearchQuery) {
            const source = activeBoardTab === "global"
              ? "global"
              : activeBoardTab === "services" ? "services" : "network";
            loadBoardSource(source, { silent: true });
          }
        });
      });
      document.querySelectorAll("[data-board-network-tab]").forEach((button) => {
        button.addEventListener("click", () => {
          activeBoardNetworkTab = boardNetworkTabs.includes(button.dataset.boardNetworkTab)
            ? button.dataset.boardNetworkTab
            : "all";
          renderBoard();
          if (!boardSearchQuery && !boardSourceState.network.loaded) loadBoardSource("network", { silent: true });
        });
      });
      const search = qs("board-search");
      if (search) {
        const runSearch = () => loadBoardSearch(search.value);
        search.addEventListener("input", () => {
          clearTimeout(boardSearchTimer);
          boardSearchTimer = window.setTimeout(runSearch, 350);
        });
        search.addEventListener("keydown", (event) => {
          if (event.key === "Enter") {
            event.preventDefault();
            clearTimeout(boardSearchTimer);
            runSearch();
          }
        });
      }
      const list = qs("board-messages");
      if (list) {
        list.addEventListener("scroll", () => {
          const nearBottom = list.scrollTop + list.clientHeight >= list.scrollHeight - 160;
          if (nearBottom) loadBoardMore();
        });
      }
      syncBoardTabs();
    }
