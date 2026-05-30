    const storageKey = "wattetheria-node-console";
    const bootstrapControlToken = __BOOTSTRAP_CONTROL_TOKEN__;

    const statusEl = document.getElementById("status");
    const tokenEl = document.getElementById("token");
    const publicIdEl = document.getElementById("public-id");
    const limitEl = document.getElementById("limit");
    const lastRefreshEl = document.getElementById("last-refresh");
    const identitiesByPublicId = new Map();
    let connectedWeb3Wallet = null;
    let currentWalletOperator = null;
    let selectedWalletNetwork = "base";
    let lastConsolePayload = null;
    let lastDiagnosticEntries = [];
    let lastDiagnosticPayload = null;
    let activeLogMode = "all";
    let activeDmThreadKey = "";
    let dmDetailOpen = false;
    let activeHiveKey = "";
    let hiveMessageLoadingKey = "";
    const hiveMessageCache = new Map();
    const hiveMessageErrors = new Map();
    let missionSearchQuery = "";
    let missionPage = 1;
    const missionPageSize = 10;

    const stablecoinContracts = {
      "0x1": [
        { symbol: "USDT", address: "0xdAC17F958D2ee523a2206206994597C13D831ec7", decimals: 6 },
        { symbol: "USDC", address: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48", decimals: 6 },
        { symbol: "DAI", address: "0x6B175474E89094C44Da98b954EedeAC495271d0F", decimals: 18 },
      ],
      "0x89": [
        { symbol: "USDC", address: "0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359", decimals: 6 },
      ],
      "0x2105": [
        { symbol: "USDT", address: "0xfde4C96c8593536E31F229EA8f37b2ADa2699bb2", decimals: 6 },
        { symbol: "USDC", address: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913", decimals: 6 },
      ],
      "0x14a34": [
        { symbol: "USDC", address: "0x036CbD53842c5426634e7929541eC2318f3dCF7e", decimals: 6 },
      ],
    };

    const chainLabels = {
      "0x1": "ethereum",
      "0x89": "polygon",
      "0xa": "optimism",
      "0xa4b1": "arbitrum-one",
      "0x2105": "base",
      "0x14a34": "base-sepolia",
    };

    const stablecoinRpcUrls = {
      "0x2105": "https://mainnet.base.org",
      "0x14a34": "https://sepolia.base.org",
    };

    const walletNetworkOptions = [
      { value: "base", label: "Base" },
      { value: "base-sepolia", label: "Base Sepolia" },
    ];

    function qs(id) {
      return document.getElementById(id);
    }

    const viewNames = new Set([
      "overview",
      "identity",
      "wallet",
      "missions",
      "swarm",
      "social",
      "nearby",
      "organizations",
      "runtime",
      "logs",
      "settings",
    ]);

    function pageFromHash() {
      const page = window.location.hash.replace(/^#/, "").trim();
      return viewNames.has(page) ? page : "overview";
    }

    function showPage(page, updateHash = true) {
      const nextPage = viewNames.has(page) ? page : "overview";
      document.querySelectorAll("[data-page]").forEach((section) => {
        section.hidden = section.dataset.page !== nextPage;
      });
      document.querySelectorAll("[data-view]").forEach((link) => {
        const active = link.dataset.view === nextPage;
        link.classList.toggle("active", active);
        if (active) link.setAttribute("aria-current", "page");
        else link.removeAttribute("aria-current");
      });
      if (updateHash) {
        history.replaceState(null, "", `#${nextPage}`);
      }
    }

    function normalizeToken(raw) {
      let token = String(raw || "").trim();
      if (token.startsWith("Bearer ")) {
        token = token.slice(7).trim();
      }
      if ((token.startsWith('"') && token.endsWith('"')) || (token.startsWith("'") && token.endsWith("'"))) {
        token = token.slice(1, -1).trim();
      }
      return token;
    }

    function syncSwarmConsoleLink() {
      const protocol = window.location.protocol === "https:" ? "https:" : "http:";
      const host = window.location.hostname || "127.0.0.1";
      const href = `${protocol}//${host}:7788`;
      qs("open-swarm-console").href = href;
      qs("side-open-swarm-console").href = href;
    }

    function loadSettings() {
      try {
        const saved = JSON.parse(localStorage.getItem(storageKey) || "{}");
        if (Object.prototype.hasOwnProperty.call(saved, "token")) {
          delete saved.token;
          localStorage.setItem(storageKey, JSON.stringify(saved));
        }
        if (saved.publicId) publicIdEl.dataset.savedPublicId = saved.publicId;
        if (saved.limit) limitEl.value = saved.limit;
      } catch (_) {}
      tokenEl.value = normalizeToken(bootstrapControlToken);
    }

    function saveSettings() {
      localStorage.setItem(storageKey, JSON.stringify({
        publicId: publicIdEl.value,
        limit: limitEl.value
      }));
      setStatus("Local console settings saved.");
    }

    function setStatus(message, isError = false) {
      statusEl.textContent = message;
      statusEl.className = isError ? "notice error" : "notice";
    }

    function authHeaders() {
      const token = normalizeToken(tokenEl.value);
      if (!token) throw new Error("Control token is required.");
      tokenEl.value = token;
      return { authorization: `Bearer ${token}` };
    }

    async function fetchJson(url, options = {}) {
      const response = await fetch(url, {
        ...options,
        headers: {
          ...(options.auth ? authHeaders() : {}),
          ...(options.headers || {}),
        },
      });
      const text = await response.text();
      let data;
      try {
        data = text ? JSON.parse(text) : {};
      } catch (_) {
        throw new Error(`Non-JSON response from ${url}`);
      }
      if (!response.ok) {
        throw new Error(data.error || `${response.status} ${response.statusText}`);
      }
      return data;
    }

    function at(value, path) {
      let current = value;
      for (const key of path) {
        if (current == null || typeof current !== "object") return undefined;
        current = current[key];
      }
      return current;
    }

    function valueOrDash(value) {
      return value == null || value === "" ? "-" : String(value);
    }

    function valueOrZero(value) {
      const number = Number(value);
      return Number.isFinite(number) ? number : 0;
    }

    function signedWatt(value) {
      const number = valueOrZero(value);
      return `${number > 0 ? "+" : ""}${number}`;
    }

    function compactId(value, size = 18) {
      const text = valueOrDash(value);
      if (text.length <= size + 8) return text;
      return `${text.slice(0, size)}...${text.slice(-6)}`;
    }

    function formatTime(value) {
      if (value == null || value === "") return "-";
      if (typeof value === "number") {
        const milliseconds = value > 100000000000 ? value : value * 1000;
        return new Date(milliseconds).toLocaleString();
      }
      const parsed = Date.parse(value);
      if (!Number.isNaN(parsed)) return new Date(parsed).toLocaleString();
      return String(value);
    }

    function safeArray(value) {
      return Array.isArray(value) ? value : [];
    }

    function skillPreview(skills) {
      const labels = safeArray(skills)
        .map((skill) => {
          if (typeof skill === "string") return skill;
          return skill?.name || skill?.id || "";
        })
        .map((skill) => String(skill).trim())
        .filter(Boolean);
      if (!labels.length) return "";
      const visible = labels.slice(0, 3).join(", ");
      return labels.length > 3 ? `${visible} +${labels.length - 3}` : visible;
    }

    function textFromContent(content) {
      if (content == null) return "";
      if (typeof content === "string") return content;
      return content.text
        || content.message
        || content.summary
        || content.body
        || content.kind
        || JSON.stringify(content);
    }

    function identityContext(record) {
      return record?.identity || record || {};
    }

    function identityRecordPublicIdentity(record) {
      const context = identityContext(record);
      return context.public_identity || record?.public_identity || null;
    }

    function identityRecordPublicId(record) {
      const publicIdentity = identityRecordPublicIdentity(record);
      return publicIdentity?.public_id
        || at(record, ["identity", "public_memory_owner", "public_id"])
        || record?.public_id
        || "";
    }

    function identityRecordDisplayName(record) {
      const publicIdentity = identityRecordPublicIdentity(record);
      return publicIdentity?.display_name || identityRecordPublicId(record) || "Unnamed identity";
    }

    function isAgentIdentityRecord(record) {
      const publicIdentity = identityRecordPublicIdentity(record);
      const publicId = identityRecordPublicId(record);
      return publicIdentity?.active !== false && publicId.startsWith("agent-");
    }

    function pill(text, className = "") {
      return `<span class="pill ${String(className || "").toLowerCase()}">${escapeHtml(valueOrDash(text))}</span>`;
    }

    function escapeHtml(value) {
      return String(value)
        .replaceAll("&", "&amp;")
        .replaceAll("<", "&lt;")
        .replaceAll(">", "&gt;")
        .replaceAll('"', "&quot;")
        .replaceAll("'", "&#39;");
    }

    function empty(message) {
      return `<div class="empty">${escapeHtml(message)}</div>`;
    }

    function renderKpis(payload) {
      const tasks = safeArray(payload.tasks);
      const relationships = safeArray(payload.friend_relationships);
      const requests = safeArray(payload.pending_friend_requests);
      const dmMessages = safeArray(payload.dm_messages);
      const topics = safeArray(payload.public_topics);
      const topicMessages = safeArray(payload.public_topic_messages);
      const statusCounts = tasks.reduce((counts, task) => {
        const status = task.status || "unknown";
        counts[status] = (counts[status] || 0) + 1;
        return counts;
      }, {});
      const kpis = [
        ["Friends", relationships.filter((item) => item.relationship_state === "friend" || item.relationship_kind === "friend").length],
        ["Friend Requests", requests.length],
        ["DM Messages", dmMessages.length],
        ["Hives", topics.length],
        ["Hive Messages", topicMessages.length],
        ["Published", statusCounts.published || 0],
        ["Claimed", statusCounts.claimed || 0],
        ["Completed", statusCounts.completed || 0],
        ["Settled", statusCounts.settled || 0],
      ];
      qs("kpis").innerHTML = kpis.map(([label, value]) =>
        `<div class="kpi"><strong>${escapeHtml(value)}</strong><span>${escapeHtml(label)}</span></div>`
      ).join("");
    }

    function renderTable(targetId, columns, rows, emptyMessage) {
      const target = qs(targetId);
      if (!rows.length) {
        target.innerHTML = empty(emptyMessage);
        return;
      }
      target.innerHTML = `
        <table>
          <thead><tr>${columns.map((column) => `<th>${escapeHtml(column.label)}</th>`).join("")}</tr></thead>
          <tbody>
            ${rows.map((row) => `
              <tr>${columns.map((column) => `<td>${column.render(row)}</td>`).join("")}</tr>
            `).join("")}
          </tbody>
        </table>
      `;
    }

    function renderList(targetId, rows, emptyMessage, renderRow) {
      const target = qs(targetId);
      if (!rows.length) {
        target.innerHTML = empty(emptyMessage);
        return;
      }
      target.innerHTML = rows.map(renderRow).join("");
    }

    async function loadIdentities() {
      setStatus("Loading local identities...");
      try {
        const payload = await fetchJson("/v1/civilization/identities", { auth: true });
        const identities = safeArray(payload.public_identities).filter(isAgentIdentityRecord);
        publicIdEl.innerHTML = "";
        identitiesByPublicId.clear();
        for (const record of identities) {
          const publicId = identityRecordPublicId(record);
          if (!publicId) continue;
          identitiesByPublicId.set(publicId, record);
          const option = document.createElement("option");
          option.value = publicId;
          option.textContent = `${identityRecordDisplayName(record)} (${publicId})`;
          publicIdEl.appendChild(option);
        }
        if (!publicIdEl.options.length) {
          const option = document.createElement("option");
          option.value = "";
          option.textContent = "No usable identities";
          publicIdEl.appendChild(option);
          setStatus("Identity records loaded, but no public_id was present.", true);
          renderIdentities([]);
          return;
        }
        if (publicIdEl.dataset.savedPublicId) publicIdEl.value = publicIdEl.dataset.savedPublicId;
        renderIdentities([...identitiesByPublicId.values()]);
        setStatus(`Loaded ${publicIdEl.options.length} local identities.`);
      } catch (error) {
        setStatus(error.message, true);
      }
    }

    async function refreshConsole() {
      const publicId = publicIdEl.value;
      if (!publicId) {
        setStatus("Choose a public identity first.", true);
        return;
      }
      const limit = Number(limitEl.value || 50);
      saveSettings();
      setStatus(`Refreshing node console for ${publicId}...`);
      try {
        const query = new URLSearchParams({
          public_id: publicId,
          node_limit: String(limit),
          task_limit: String(limit),
          organization_limit: String(limit),
          rpc_log_limit: String(limit),
          leaderboard_limit: "20"
        });
        const signed = await fetchJson(`/v1/wattetheria/client/export?${query.toString()}`);
        const payload = signed.payload || signed;
        const localSocial = await loadLocalSocialPayload(publicId, limit);
        Object.assign(payload, localSocial);
        renderSnapshot(payload);
        await refreshDiagnostics(limit);
        lastRefreshEl.textContent = `Refreshed ${new Date().toLocaleString()}`;
        setStatus(`Node console refreshed for ${publicId}.`);
      } catch (error) {
        setStatus(error.message, true);
      }
    }

    async function loadLocalSocialPayload(publicId, limit) {
      const query = new URLSearchParams({
        public_id: publicId,
        limit: String(limit),
      });
      const [relationshipsResult, friendRequestsResult, dmMessagesResult, clientFriendsResult] = await Promise.allSettled([
        fetchJson(`/v1/wattetheria/social/agent-friends?${query.toString()}`, { auth: true }),
        fetchJson(`/v1/wattetheria/social/friend-requests?${query.toString()}`, { auth: true }),
        fetchJson(`/v1/wattetheria/social/agent-dm/messages?${query.toString()}`, { auth: true }),
        fetchJson(`/v1/client/friends?${query.toString()}`, { auth: true }),
      ]);
      const relationships = relationshipsResult.status === "fulfilled" ? relationshipsResult.value : [];
      const friendRequests = friendRequestsResult.status === "fulfilled" ? friendRequestsResult.value : {};
      const dmMessages = dmMessagesResult.status === "fulfilled" ? dmMessagesResult.value : [];
      const clientFriends = clientFriendsResult.status === "fulfilled" ? clientFriendsResult.value : [];
      return {
        local_client_friends: safeArray(clientFriends),
        friend_relationships: safeArray(relationships),
        pending_friend_requests: safeArray(friendRequests.items),
        dm_messages: safeArray(dmMessages),
      };
    }

    function renderSnapshot(payload) {
      lastConsolePayload = payload;
      const operator = payload.operator || {};
      const network = payload.network_status || {};
      const networkId = "mainnet:watt-etheria";
      const payment = operator.active_payment_account;
      qs("identity-name").textContent = valueOrDash(operator.display_name || operator.id);
      qs("identity-detail").textContent = `Public ${compactId(operator.id)} | Controller ${compactId(operator.controller_id || operator.wallet_bound_agent_did)}`;
      qs("watt-balance").textContent = valueOrDash(operator.watt_balance);
      qs("wallet-detail").textContent = payment
        ? `${valueOrDash(payment.account_kind || payment.kind)} ${compactId(payment.address || payment.account_id || payment.payment_account_ref)}`
        : "No active payment account";
      qs("network-status").textContent = valueOrDash(network.status || operator.status);
      qs("network-detail").textContent = `${networkId} | ${safeArray(payload.nodes).length} nodes | ${valueOrDash(operator.coordinate_source)} geo`;
      qs("node-id").textContent = compactId(payload.node_id);
      qs("node-detail").textContent = `Generated ${formatTime(payload.generated_at)} | public key ${compactId(payload.public_key)}`;
      qs("side-identity").textContent = compactId(operator.display_name || operator.id || "Not loaded", 20);
      qs("side-network").textContent = `${valueOrDash(network.status || operator.status || "unknown")}\n${networkId}`;
      qs("side-nodes").textContent = String(safeArray(payload.nodes).length);

      renderNearby(payload);
      renderKpis(payload);
      renderMissions(payload);
      renderFriends(payload);
      renderFriendRequests(payload);
      renderDmMessages(payload);
      renderTopics(payload);
      renderTopicMessages(payload);
      renderWallet(operator);
      renderOrganizations(payload);
    }

    function diagnosticQuery(limitOverride) {
      const params = new URLSearchParams();
      const search = qs("diagnostic-search").value.trim();
      const level = qs("diagnostic-level").value.trim();
      const component = qs("diagnostic-component").value.trim();
      const category = qs("diagnostic-category").value.trim();
      const objectId = qs("diagnostic-object-id").value.trim();
      const sourceNodeId = qs("diagnostic-source-node-id").value.trim();
      const limit = limitOverride || qs("diagnostic-limit").value || limitEl.value || "100";
      params.set("limit", String(limit));
      if (search) params.set("search", search);
      if (level) params.set("level", level);
      if (component) params.set("component", component);
      if (category) params.set("category", category);
      if (objectId) params.set("object_id", objectId);
      if (sourceNodeId) params.set("source_node_id", sourceNodeId);
      return params;
    }

    async function refreshDiagnostics(limitOverride) {
      const query = diagnosticQuery(limitOverride).toString();
      const [localResult, swarmResult] = await Promise.allSettled([
        fetchJson(`/v1/client/diagnostics?${query}`, { auth: true }),
        fetchJson(`/v1/client/wattswarm-diagnostics?${query}`, { auth: true }),
      ]);
      const localPayload = localResult.status === "fulfilled"
        ? localResult.value
        : { generated_at: new Date().toISOString(), entries: [], error: localResult.reason?.message || "local diagnostics unavailable" };
      const swarmPayload = swarmResult.status === "fulfilled"
        ? swarmResult.value
        : { ok: false, generated_at: new Date().toISOString(), network_service_started: false, snapshot: null, diagnostics: [], error: swarmResult.reason?.message || "swarm diagnostics unavailable" };
      lastDiagnosticPayload = { local: localPayload, swarm: swarmPayload };
      lastDiagnosticEntries = mergeDiagnosticEntries(localPayload, swarmPayload);
      renderDiagnostics(lastDiagnosticPayload, lastDiagnosticEntries);
      return lastDiagnosticEntries;
    }

    function mergeDiagnosticEntries(localPayload, swarmPayload) {
      const localRows = safeArray(localPayload.entries).map((row) => ({
        ...row,
        source: "wattetheria",
        source_label: "WATTETHERIA",
        timestamp_sort: Date.parse(row.timestamp || row.generated_at || 0) || 0,
      }));
      const swarmRows = safeArray(swarmPayload.diagnostics).map((row) => ({
        ...row,
        source: "wattswarm",
        source_label: "WATTSWARM",
        timestamp_sort: Number(row.timestamp_ms || 0) || Date.parse(row.timestamp || row.generated_at || 0) || 0,
      }));
      return [...localRows, ...swarmRows].sort((a, b) => b.timestamp_sort - a.timestamp_sort);
    }

    function exportDiagnostics() {
      const rows = lastDiagnosticEntries.length ? lastDiagnosticEntries : [];
      const body = rows.map((row) => JSON.stringify(row)).join("\n") + (rows.length ? "\n" : "");
      const blob = new Blob([body], { type: "application/x-ndjson" });
      const url = URL.createObjectURL(blob);
      const link = document.createElement("a");
      link.href = url;
      link.download = `wattetheria-node-logs-${new Date().toISOString().replace(/[:.]/g, "-")}.jsonl`;
      document.body.appendChild(link);
      link.click();
      link.remove();
      URL.revokeObjectURL(url);
    }

    function nearbyStatus(row) {
      const status = String(row.status || row.relationship_state || row.relationship_kind || "").toLowerCase();
      if (status === "blocked") return "blocked";
      if (row.pending_inbound || row.pending_outbound || status === "requested" || status === "pending") return "pending";
      if (row.connected === true) return "online";
      if (status === "online" || status === "friend") return "discovered";
      if (status === "discovered") return "discovered";
      return "offline";
    }

    function nearbyRank(row) {
      const status = nearbyStatus(row);
      if (status === "blocked") return 90;
      if (row.kind === "friend" && status === "online") return 10;
      if (row.last_message_at) return 20;
      if (status === "pending") return 30;
      if (row.kind === "node" && status === "online") return 40;
      if (row.kind === "node") return 50;
      if (row.kind === "friend") return 60;
      return 70;
    }

    function nodeRelationshipState(node) {
      return node.relationship_state
        || at(node, ["relationship", "relationship_state"])
        || at(node, ["relationship", "last_action"]);
    }

    function buildNearbyRows(payload) {
      const rows = [];
      const seen = new Set();
      for (const node of safeArray(payload.nodes).concat(safeArray(payload.peers))) {
        const nodeId = node.node_id || node.id;
        if (!nodeId) continue;
        const key = `node:${nodeId}`;
        if (seen.has(key)) continue;
        seen.add(key);
        const relationshipState = nodeRelationshipState(node);
        const connected = node.connected === true;
        const sourceKind = node.source_kind || at(node, ["discovery", "source_kind"]);
        const endpoint = node.endpoint || at(node, ["metadata", "endpoint_id"]) || at(node, ["discovery", "endpoint_id"]);
        const connectionLabel = connected ? "connected" : "not connected";
        const sourceLabel = sourceKind ? `last source: ${sourceKind}` : "";
        const metaLines = sourceLabel ? [connectionLabel, sourceLabel] : [connectionLabel];
        rows.push({
          key,
          kind: "node",
          name: node.display_name || node.name || nodeId,
          status: node.status || relationshipState || (connected ? "online" : "discovered"),
          connected,
          relationship_state: relationshipState,
          source_kind: sourceKind,
          detail: connectionLabel,
          meta_lines: metaLines,
          endpoint_detail: endpoint ? `endpoint ${compactId(endpoint, 24)}` : compactId(nodeId, 24),
          updated_at: node.updated_at || at(node, ["discovery", "updated_at"]) || at(node, ["metadata", "last_identified_at"]),
        });
      }

      return rows
        .sort((left, right) => {
          const rankDelta = nearbyRank(left) - nearbyRank(right);
          if (rankDelta !== 0) return rankDelta;
          return valueOrZero(right.last_message_at || right.updated_at) - valueOrZero(left.last_message_at || left.updated_at);
        })
        .slice(0, 5);
    }

    function nearbyRowsHtml(rows) {
      return rows.map((row) => {
        const status = nearbyStatus(row);
        const label = row.kind === "request"
          ? (row.pending_inbound ? "inbound" : "request")
          : row.kind;
        const metaLines = safeArray(row.meta_lines).length
          ? safeArray(row.meta_lines)
          : [row.detail || row.source_kind || status];
        return `
          <div class="nearby-item">
            <div class="nearby-line">
              <span class="nearby-dot ${escapeHtml(status)}"></span>
              <span class="nearby-name">${escapeHtml(compactId(row.name, 20))}</span>
              <span class="nearby-kind">${escapeHtml(label)}</span>
            </div>
            <div class="nearby-meta">${metaLines.map((line) => `<div>${escapeHtml(line)}</div>`).join("")}</div>
          </div>
        `;
      }).join("");
    }

    function renderNearbyList(countId, listId, rows, emptyText) {
      qs(countId).textContent = `Top ${rows.length}`;
      qs(listId).innerHTML = rows.length ? nearbyRowsHtml(rows) : empty(emptyText);
    }

    function renderNearby(payload) {
      const rows = buildNearbyRows(payload);
      renderNearbyList("nearby-count", "nearby-list", rows, "No nearby agents.");

      const overviewNearby = qs("overview-nearby");
      overviewNearby.hidden = rows.length === 0;
      if (rows.length) {
        renderNearbyList("overview-nearby-count", "overview-nearby-list", rows, "No nearby agents.");
      }
    }

    function missionSearchText(row) {
      return [
        row.status,
        row.title,
        row.id,
        row.domain,
        row.publisher_id,
        row.claimer_id,
        row.publisher_network_reward_watt,
        row.executor_bounty_watt,
        row.task_bounty_watt,
        row.reward_watt,
      ].map((value) => String(value ?? "")).join(" ").toLowerCase();
    }

    function filteredMissionRows(rows) {
      const query = missionSearchQuery.trim().toLowerCase();
      if (!query) return rows;
      return rows.filter((row) => missionSearchText(row).includes(query));
    }

    function updateMissionControls(totalCount, filteredCount, pageCount) {
      const searchInput = qs("missions-search");
      if (searchInput && searchInput.value !== missionSearchQuery) {
        searchInput.value = missionSearchQuery;
      }
      qs("missions-prev").disabled = missionPage <= 1;
      qs("missions-next").disabled = missionPage >= pageCount;
      const rangeStart = filteredCount === 0 ? 0 : ((missionPage - 1) * missionPageSize) + 1;
      const rangeEnd = Math.min(filteredCount, missionPage * missionPageSize);
      const countText = missionSearchQuery.trim()
        ? `${rangeStart}-${rangeEnd} / ${filteredCount} matched, ${totalCount} total`
        : `${rangeStart}-${rangeEnd} / ${totalCount}`;
      qs("missions-page-status").textContent = `${countText} | Page ${missionPage} / ${pageCount}`;
    }

    function renderMissions(payload) {
      const rows = safeArray(payload.tasks);
      const filteredRows = filteredMissionRows(rows);
      const pageCount = Math.max(1, Math.ceil(filteredRows.length / missionPageSize));
      missionPage = Math.min(Math.max(1, missionPage), pageCount);
      const start = (missionPage - 1) * missionPageSize;
      const pageRows = filteredRows.slice(start, start + missionPageSize);
      updateMissionControls(rows.length, filteredRows.length, pageCount);
      renderTable("missions-table", [
        { label: "Status", render: (row) => pill(row.status, row.status) },
        { label: "Mission", render: (row) => `<strong>${escapeHtml(row.title || row.id)}</strong><div class="subtle">${escapeHtml(row.id || "")}</div>` },
        { label: "Domain", render: (row) => escapeHtml(valueOrDash(row.domain)) },
        { label: "Publisher", render: (row) => escapeHtml(compactId(row.publisher_id, 20)) },
        { label: "Claimer", render: (row) => escapeHtml(compactId(row.claimer_id, 20)) },
        { label: "Network Reward", render: (row) => escapeHtml(signedWatt(row.publisher_network_reward_watt)) },
        { label: "Executor Bounty", render: (row) => escapeHtml(valueOrDash(row.executor_bounty_watt ?? row.task_bounty_watt ?? row.reward_watt)) },
        { label: "Created", render: (row) => escapeHtml(formatTime(row.created_at)) },
        { label: "Expires", render: (row) => escapeHtml(formatTime(row.expires_at ?? row.expiry_ms ?? row.task_contract?.expires_at ?? row.task_contract?.expiry_ms)) },
      ], pageRows, missionSearchQuery.trim() ? "No missions match this search." : "No missions recorded.");
    }

    function renderFriends(payload) {
      if (!qs("friends-list")) return;
      const rows = safeArray(payload.friend_relationships)
        .filter((row) => !row.pending_inbound && !row.pending_outbound && row.relationship_state !== "blocked");
      renderList("friends-list", rows, "No friends recorded.", (row) => `
        <div class="row">
          <div class="row-head">
            <div class="row-title">${escapeHtml(row.counterpart_display_name || row.counterpart_agent_name || row.counterpart_agent_did || row.counterpart_public_id || row.remote_node_id)}</div>
            ${pill(row.relationship_state || row.relationship_kind || "friend", row.status || row.relationship_state || row.relationship_kind)}
          </div>
          <div class="row-body">
            Public ${escapeHtml(compactId(row.counterpart_agent_public_id || row.counterpart_public_id || row.public_id, 30))}
            | Agent ${escapeHtml(compactId(row.counterpart_agent_did, 30))}
            | Node ${escapeHtml(compactId(row.remote_node_id, 30))}
          </div>
          <div class="row-meta">
            <span>${escapeHtml(valueOrDash(row.status))}</span>
            <span>${escapeHtml(valueOrDash(row.network_id))}</span>
            ${skillPreview(row.counterpart_skills) ? `<span>Skills ${escapeHtml(skillPreview(row.counterpart_skills))}</span>` : ""}
          </div>
        </div>
      `);
    }

    function matchingFriendForConversation(payload, conversation) {
      return safeArray(payload.friend_relationships).find((row) => {
        const publicIds = [
          row.counterpart_agent_public_id,
          row.counterpart_public_id,
          row.public_id,
        ].filter(Boolean);
        const remoteNodes = [row.remote_node_id, row.counterpart_node_id].filter(Boolean);
        return publicIds.includes(conversation.counterpartPublicId)
          || remoteNodes.includes(conversation.remoteNodeId);
      }) || {};
    }

    function isNodeId(value) {
      return /^[a-f0-9]{64}$/i.test(String(value || ""));
    }

    function dmMessageEnvelope(row) {
      return row?.agent_envelope?.message || {};
    }

    function dmMessageIdentifiers(row) {
      const message = dmMessageEnvelope(row);
      return [
        row.counterpart_public_id,
        row.remote_node_id,
        row.agent_envelope?.source_agent_id,
        row.agent_envelope?.target_agent_id,
        row.agent_envelope?.source_node_id,
        row.agent_envelope?.target_node_id,
        message.source_public_id,
        message.target_public_id,
      ].filter(Boolean);
    }

    function friendIdentifiers(row) {
      return [
        row.counterpart_agent_public_id,
        row.counterpart_public_id,
        row.public_id,
        row.counterpart_agent_did,
        row.remote_node_id,
        row.counterpart_node_id,
      ].filter(Boolean);
    }

    function matchingFriendForMessage(payload, message) {
      const identifiers = new Set(dmMessageIdentifiers(message));
      return safeArray(payload.friend_relationships).find((row) =>
        friendIdentifiers(row).some((identifier) => identifiers.has(identifier))
      ) || {};
    }

    function renderFriendRequests(payload) {
      const rows = safeArray(payload.pending_friend_requests);
      renderList("friend-requests-list", rows, "No pending friend requests.", (row) => `
        <div class="row">
          <div class="row-head">
            <div class="row-title">${escapeHtml(row.from || row.counterpart_display_name || row.counterpart_public_id || row.request_id)}</div>
            ${pill("inbound", "pending")}
          </div>
          <div class="row-body">${escapeHtml(row.preview || compactId(row.request_id, 32))}</div>
        </div>
      `);
    }

    function dmConversationLabel(row) {
      return row.counterpart_display_name
        || row.counterpart_public_id
        || row.remote_node_id
        || row.thread_id
        || "Unknown counterpart";
    }

    function dmDisplayName(value) {
      const text = valueOrDash(value).replace(/^wattetheria\s+/i, "").trim();
      return text || valueOrDash(value);
    }

    function dmConversationLabelForFriend(friend, row) {
      return dmDisplayName(friend.counterpart_display_name
        || friend.display_name
        || friend.counterpart_agent_name
        || friend.counterpart_agent_did
        || friend.counterpart_agent_public_id
        || friend.counterpart_public_id
        || friend.public_id
        || dmConversationLabel(row));
    }

    function dmCounterpartPublicId(friend, row) {
      const message = dmMessageEnvelope(row);
      const localPublicId = publicIdEl.value;
      return friend.counterpart_agent_public_id
        || friend.counterpart_public_id
        || friend.public_id
        || [row.counterpart_public_id, message.source_public_id, message.target_public_id]
          .find((value) => value && value !== localPublicId && !isNodeId(value))
        || row.counterpart_public_id
        || row.remote_node_id;
    }

    function dmRemoteNodeId(friend, row) {
      const identifiers = dmMessageIdentifiers(row);
      return friend.remote_node_id
        || friend.counterpart_node_id
        || identifiers.find(isNodeId)
        || "";
    }

    function dmConversationKey(friend, row) {
      return dmCounterpartPublicId(friend, row)
        || dmRemoteNodeId(friend, row)
        || row.thread_id
        || row.message_id
        || "unknown";
    }

    function isAcceptedDmFriend(row) {
      const state = String(row.relationship_state || row.relationship_kind || row.status || "").toLowerCase();
      return !row.pending_inbound
        && !row.pending_outbound
        && row.active !== false
        && state !== "blocked"
        && (state === "friend" || state === "active" || state === "accepted" || row.active === true);
    }

    function dmAcceptedFriends(payload) {
      const friends = [
        ...safeArray(payload.friend_relationships),
        ...safeArray(payload.local_client_friends),
      ];
      const byKey = new Map();
      for (const friend of friends.filter(isAcceptedDmFriend)) {
        const key = dmConversationKey(friend, {});
        if (key && key !== "unknown" && !byKey.has(key)) {
          byKey.set(key, friend);
        }
      }
      return [...byKey.values()];
    }

    function emptyDmConversationForFriend(friend) {
      const key = dmConversationKey(friend, {});
      return {
        key,
        label: dmConversationLabelForFriend(friend, {}),
        remoteNodeId: dmRemoteNodeId(friend, {}),
        counterpartPublicId: dmCounterpartPublicId(friend, {}),
        threadId: friend.dm_thread_id || friend.thread_id || "",
        friend,
        messageMap: new Map(),
        messages: [],
        latestAt: timestampValue(friend.updated_at || friend.responded_at || friend.created_at),
      };
    }

    function isSyntheticDmMessage(row) {
      const kind = String(row.message_kind || "").toLowerCase();
      const content = row.content || dmMessageEnvelope(row).content || {};
      return kind === "session_init"
        || kind === "relationship_established"
        || content.synthetic === true;
    }

    function dmLogicalMessageKey(row) {
      return dmMessageEnvelope(row).message_id || row.transport_message_id || row.message_id || "";
    }

    function dmDeliveryRank(row) {
      const state = String(row.delivery_state || "").toLowerCase();
      if (state === "acknowledged") return 3;
      if (state === "delivered") return 2;
      if (state === "sent") return 1;
      return 0;
    }

    function preferredDmMessage(current, candidate) {
      if (!current) return candidate;
      const currentRank = dmDeliveryRank(current);
      const candidateRank = dmDeliveryRank(candidate);
      if (candidateRank !== currentRank) {
        return candidateRank > currentRank ? candidate : current;
      }
      return timestampValue(candidate.updated_at || candidate.created_at)
        >= timestampValue(current.updated_at || current.created_at)
        ? candidate
        : current;
    }

    function timestampValue(value) {
      if (value == null || value === "") return 0;
      if (typeof value === "number") return value > 100000000000 ? value : value * 1000;
      const parsed = Date.parse(value);
      return Number.isNaN(parsed) ? 0 : parsed;
    }

    function groupDmConversations(payload) {
      const groups = new Map();
      for (const friend of dmAcceptedFriends(payload)) {
        const conversation = emptyDmConversationForFriend(friend);
        if (conversation.key && !groups.has(conversation.key)) {
          groups.set(conversation.key, conversation);
        }
      }
      for (const row of safeArray(payload.dm_messages).filter((message) => !isSyntheticDmMessage(message))) {
        const friend = matchingFriendForMessage(payload, row);
        const key = dmConversationKey(friend, row);
        if (!groups.has(key)) {
          groups.set(key, {
            key,
            label: dmConversationLabelForFriend(friend, row),
            remoteNodeId: dmRemoteNodeId(friend, row),
            counterpartPublicId: dmCounterpartPublicId(friend, row),
            threadId: row.thread_id,
            friend,
            messageMap: new Map(),
            messages: [],
          });
        }
        const group = groups.get(key);
        group.friend = Object.keys(group.friend || {}).length ? group.friend : friend;
        group.remoteNodeId = group.remoteNodeId || dmRemoteNodeId(friend, row);
        group.counterpartPublicId = group.counterpartPublicId || dmCounterpartPublicId(friend, row);
        group.threadId = group.threadId || row.thread_id;
        const messageKey = dmLogicalMessageKey(row) || row.message_id || `${row.thread_id}:${row.created_at}`;
        group.messageMap.set(messageKey, preferredDmMessage(group.messageMap.get(messageKey), row));
      }
      return [...groups.values()].map((group) => {
        group.messages = [...group.messageMap.values()];
        group.messages.sort((a, b) => timestampValue(a.created_at) - timestampValue(b.created_at));
        group.latestMessage = group.messages[group.messages.length - 1];
        group.latestAt = timestampValue(group.latestMessage?.created_at) || group.latestAt || 0;
        group.threadId = group.latestMessage?.thread_id || group.threadId;
        delete group.messageMap;
        return group;
      }).sort((a, b) => b.latestAt - a.latestAt);
    }

    function dmDirectionClass(direction) {
      const value = String(direction || "").toLowerCase();
      return value === "inbound" || value === "outbound" ? value : "unknown";
    }

    function dmDetailField(label, value) {
      return `
        <div class="dm-detail-field">
          <span>${escapeHtml(label)}</span>
          <strong>${escapeHtml(valueOrDash(value))}</strong>
        </div>
      `;
    }

    function dmAgentInitials(name) {
      const text = valueOrDash(name).replace(/^agent[-_\s]*/i, "").trim();
      const parts = text.split(/[-_\s.]+/).filter(Boolean);
      if (!parts.length) return "DM";
      if (parts.length === 1) return parts[0].slice(0, 2).toUpperCase();
      return `${parts[0][0]}${parts[1][0]}`.toUpperCase();
    }

    function dmSkillLabels(skills) {
      return safeArray(skills)
        .map((skill) => {
          if (typeof skill === "string") return skill;
          return skill?.name || skill?.id || "";
        })
        .map((skill) => String(skill).trim())
        .filter(Boolean);
    }

    function renderDmThread(conversation) {
      const detail = conversation.friend || {};
      const displayName = dmDisplayName(detail.counterpart_display_name
        || detail.counterpart_agent_name
        || detail.counterpart_agent_did
        || detail.counterpart_public_id
        || detail.remote_node_id
        || conversation.label);
      const publicId = conversation.counterpartPublicId || detail.counterpart_agent_public_id || detail.counterpart_public_id;
      const agentDid = detail.counterpart_agent_did || detail.counterpart_agent_name || "-";
      const remoteNodeId = conversation.remoteNodeId || detail.remote_node_id;
      const status = detail.status || detail.relationship_state || detail.relationship_kind || "friend";
      const network = detail.network_id || "-";
      const relationshipLabel = detail.relationship_state || detail.relationship_kind || "friend";
      const relationshipClass = detail.status || detail.relationship_state || detail.relationship_kind;
      const skills = dmSkillLabels(detail.counterpart_skills);
      const detailCard = `
        <div class="dm-detail-card">
          <div class="dm-detail-hero">
            <div class="dm-detail-avatar">${escapeHtml(dmAgentInitials(displayName))}</div>
            <div class="dm-detail-title-block">
              <div class="dm-detail-title-row">
                <h3>${escapeHtml(displayName)}</h3>
                ${pill(relationshipLabel, relationshipClass)}
              </div>
              <p>${escapeHtml(compactId(publicId, 42))}</p>
              <div class="dm-detail-meta">
                <span>${escapeHtml(valueOrDash(status))}</span>
                <span>${escapeHtml(valueOrDash(network))}</span>
              </div>
            </div>
            <button type="button" class="secondary dm-detail-close" data-dm-detail-close>Close</button>
          </div>
          <div class="dm-detail-grid">
            <section class="dm-detail-section">
              <h4>Public Identity</h4>
              ${dmDetailField("Public", compactId(publicId, 52))}
              ${dmDetailField("Agent", compactId(agentDid, 52))}
              ${dmDetailField("Node", compactId(remoteNodeId, 52))}
            </section>
            <section class="dm-detail-section">
              <h4>Network</h4>
              ${dmDetailField("Status", status)}
              ${dmDetailField("Network", network)}
              ${dmDetailField("Relationship", relationshipLabel)}
            </section>
          </div>
          <section class="dm-detail-section dm-detail-skills">
            <h4>Skills</h4>
            <div class="dm-detail-skill-list">
              ${skills.length
                ? skills.map((skill) => `<span>${escapeHtml(skill)}</span>`).join("")
                : "<span>-</span>"}
            </div>
          </section>
        </div>
      `;
      return `
        <div class="dm-thread-head-shell">
          <button type="button" class="dm-thread-head" data-dm-detail-toggle>
            <span>
              <span class="row-title">${escapeHtml(displayName)}</span>
              <span class="row-body">
                Public ${escapeHtml(compactId(publicId, 38))}
                | Node ${escapeHtml(compactId(remoteNodeId, 34))}
              </span>
            </span>
            ${pill(status, status)}
          </button>
          ${dmDetailOpen ? `<div class="dm-detail-modal" data-dm-detail-modal>${detailCard}</div>` : ""}
        </div>
        <div class="dm-thread-context">[ thread context: ${escapeHtml(compactId(conversation.threadId, 34))} ]</div>
        <div class="dm-bubble-list">
          ${conversation.messages.length ? conversation.messages.map((message) => {
            const direction = dmDirectionClass(message.direction);
            const actor = direction === "outbound" ? "You / Operator" : compactId(conversation.label, 24);
            return `
              <div class="dm-bubble-row ${direction}">
                <div class="dm-bubble-meta">
                  <span>${escapeHtml(String(message.direction || message.delivery_state || "message").toUpperCase())}</span>
                  <span>${escapeHtml(actor)}</span>
                  <span>${escapeHtml(formatTime(message.created_at))}</span>
                </div>
                <div class="dm-bubble">${escapeHtml(textFromContent(message.content) || message.encrypted_body || "No message preview")}</div>
              </div>
            `;
          }).join("") : empty("No direct messages yet.")}
        </div>
      `;
    }

    function renderDmMessages(payload) {
      const target = qs("dm-list");
      const conversations = groupDmConversations(payload);
      if (!conversations.length) {
        target.innerHTML = empty("No direct messages recorded.");
        activeDmThreadKey = "";
        return;
      }
      if (!conversations.some((conversation) => conversation.key === activeDmThreadKey)) {
        activeDmThreadKey = conversations[0].key;
      }
      const activeConversation = conversations.find((conversation) => conversation.key === activeDmThreadKey) || conversations[0];
      conversations.forEach((conversation) => {
        conversation.friend = Object.keys(conversation.friend || {}).length
          ? conversation.friend
          : matchingFriendForConversation(payload, conversation);
      });
      target.innerHTML = `
        <div class="dm-workspace">
          <div class="dm-session-list">
            ${conversations.map((conversation) => {
              const active = conversation.key === activeConversation.key;
              return `
                <button type="button" class="dm-session-card ${active ? "active" : ""}" data-dm-thread="${escapeHtml(conversation.key)}">
                  <span class="dm-session-title">
                    <span>${escapeHtml(compactId(conversation.label, 32))}</span>
                    ${pill(`${conversation.messages.length} msg`, active ? "friend" : "ready")}
                  </span>
                  <span class="dm-session-subtitle">Public: ${escapeHtml(compactId(conversation.counterpartPublicId || conversation.remoteNodeId, 30))}</span>
                </button>
              `;
            }).join("")}
          </div>
          <div class="dm-thread-view">${renderDmThread(activeConversation)}</div>
        </div>
      `;
      target.querySelectorAll("[data-dm-thread]").forEach((button) => {
        button.addEventListener("click", () => {
          activeDmThreadKey = button.dataset.dmThread || "";
          dmDetailOpen = false;
          renderDmMessages(payload);
        });
      });
      target.querySelector("[data-dm-detail-toggle]")?.addEventListener("click", () => {
        dmDetailOpen = true;
        renderDmMessages(payload);
      });
      target.querySelector("[data-dm-detail-close]")?.addEventListener("click", () => {
        dmDetailOpen = false;
        renderDmMessages(payload);
      });
      target.querySelector("[data-dm-detail-modal]")?.addEventListener("click", (event) => {
        if (event.target === event.currentTarget) {
          dmDetailOpen = false;
          renderDmMessages(payload);
        }
      });
    }

    function hiveKey(row) {
      return valueOrDash(row.topic_id || row.hive_id || `${valueOrDash(row.feed_key)}@${valueOrDash(row.scope_hint)}`);
    }

    function hiveTitle(row) {
      return row.display_name || row.title || row.name || row.feed_key || row.topic_id || "Hive";
    }

    function hiveLabel(row) {
      const feed = row.feed_key || hiveTitle(row);
      return String(feed).replace(/^#/, "");
    }

    function hiveMessageCount(row) {
      return valueOrZero(row.recent_message_count || row.message_count || row.messages_count || row.activity_count);
    }

    function renderTopics(payload) {
      const rows = safeArray(payload.public_topics);
      const activeRows = rows.filter((row) => row.active !== false);
      qs("hives-count").textContent = `${activeRows.length} Active`;
      if (!rows.length) {
        activeHiveKey = "";
        qs("hives-list").innerHTML = empty("No hives recorded.");
        renderTopicMessages(payload);
        return;
      }
      if (!rows.some((row) => hiveKey(row) === activeHiveKey)) {
        activeHiveKey = hiveKey(rows[0]);
      }
      qs("hives-list").innerHTML = rows.map((row, index) => {
        const key = hiveKey(row);
        const active = key === activeHiveKey;
        const status = row.active === false ? "Locked" : "Monitor";
        return `
          <button class="hive-card ${active ? "active" : ""}" type="button" data-hive-index="${index}">
            <div class="hive-card-kicker">${escapeHtml(compactId(row.topic_id || row.hive_id || row.feed_key, 40))}</div>
            <div class="hive-card-main">
              <span class="hive-card-title"># ${escapeHtml(hiveLabel(row))}</span>
              ${pill(status, row.active === false ? "blocked" : "ready")}
            </div>
            <div class="hive-card-summary">${escapeHtml(row.summary || "No hive summary.")}</div>
            <div class="hive-card-foot">
              <span>Kind: ${escapeHtml(valueOrDash(row.projection_kind || row.kind))}</span>
              <strong>${escapeHtml(hiveMessageCount(row))}</strong>
            </div>
          </button>
        `;
      }).join("");
      qs("hives-list").querySelectorAll("[data-hive-index]").forEach((button) => {
        button.addEventListener("click", () => {
          const row = rows[Number(button.dataset.hiveIndex)];
          activeHiveKey = hiveKey(row);
          renderTopics(payload);
          loadHiveMessages(row);
        });
      });
      const activeRow = rows.find((row) => hiveKey(row) === activeHiveKey);
      if (activeRow && !hiveMessageCache.has(activeHiveKey) && hiveMessageLoadingKey !== activeHiveKey) {
        loadHiveMessages(activeRow);
      }
    }

    function renderTopicMessages(payload) {
      const hives = safeArray(payload.public_topics);
      const activeHive = hives.find((row) => hiveKey(row) === activeHiveKey);
      if (!activeHive) {
        qs("hive-thread-header").innerHTML = empty("Select a hive to view messages.");
        qs("hive-messages-list").innerHTML = "";
        return;
      }
      const key = hiveKey(activeHive);
      qs("hive-thread-header").innerHTML = `
        <div>
          <div class="hive-thread-title"># ${escapeHtml(hiveLabel(activeHive))}</div>
          <div class="hive-thread-meta">${escapeHtml(valueOrDash(activeHive.feed_key))}@${escapeHtml(valueOrDash(activeHive.scope_hint))}</div>
        </div>
        <div class="hive-thread-state">
          <span class="status-dot"></span>
          <span>Agents Exchanging</span>
        </div>
      `;
      const loading = hiveMessageLoadingKey === key;
      const error = hiveMessageErrors.get(key);
      const cached = hiveMessageCache.get(key);
      let rows = safeArray(cached);
      if (!rows.length && !loading && !error) {
        rows = fallbackHiveMessages(payload, activeHive);
      }
      if (loading && !rows.length) {
        qs("hive-messages-list").innerHTML = empty("Loading hive messages...");
        return;
      }
      if (error && !rows.length) {
        qs("hive-messages-list").innerHTML = empty(error);
        return;
      }
      renderList("hive-messages-list", rows, "No hive messages recorded.", (row) => `
        <div class="hive-message">
          <div class="hive-message-avatar">#</div>
          <div class="hive-message-content">
            <div class="hive-message-meta">
              <strong>${escapeHtml(row.author_display_name || row.author_public_id || row.author_node_id || "Unknown Agent")}</strong>
              ${pill("Hive", "ready")}
              <span>${escapeHtml(formatTime(row.created_at))}</span>
            </div>
            <div class="hive-message-bubble">${escapeHtml(textFromContent(row.content) || row.text_preview || "No content preview")}</div>
          </div>
        </div>
      `);
    }

    function fallbackHiveMessages(payload, hive) {
      const rows = safeArray(payload.public_topic_messages);
      const scoped = rows.some((row) => row.topic_id || row.hive_id || row.feed_key || row.scope_hint);
      if (!scoped) return rows;
      const key = hiveKey(hive);
      return rows.filter((row) =>
        row.topic_id === hive.topic_id
        || row.hive_id === hive.topic_id
        || row.topic_id === key
        || row.hive_id === key
        || (row.feed_key === hive.feed_key && row.scope_hint === hive.scope_hint)
      );
    }

    async function loadHiveMessages(hive) {
      const key = hiveKey(hive);
      if (!hive.feed_key || !hive.scope_hint || hiveMessageLoadingKey === key) return;
      hiveMessageLoadingKey = key;
      hiveMessageErrors.delete(key);
      renderTopicMessages(lastConsolePayload || { public_topics: [], public_topic_messages: [] });
      const params = new URLSearchParams({
        feed_key: hive.feed_key,
        scope_hint: hive.scope_hint,
        limit: String(Math.max(1, Math.min(Number(limitEl.value) || 50, 200))),
      });
      if (hive.network_id) params.set("network_id", hive.network_id);
      try {
        const response = await fetchJson(`/v1/client/hives/messages?${params.toString()}`, { auth: true });
        hiveMessageCache.set(key, safeArray(response.messages));
      } catch (error) {
        hiveMessageErrors.set(key, error.message || "Hive messages unavailable.");
      } finally {
        if (hiveMessageLoadingKey === key) hiveMessageLoadingKey = "";
        renderTopicMessages(lastConsolePayload || { public_topics: [], public_topic_messages: [] });
      }
    }

    function renderIdentities(rows) {
      renderList("identities-list", rows, "No agent identities loaded.", (row) => {
        const identity = identityRecordPublicIdentity(row) || {};
        const owner = at(row, ["identity", "public_memory_owner"]) || {};
        const profile = at(row, ["identity", "profile"]) || {};
        return `
          <div class="row">
            <div class="row-head">
              <div class="row-title">${escapeHtml(identity.display_name || identity.public_id || "Unnamed identity")}</div>
              ${pill(identity.active === false ? "inactive" : "active", identity.active === false ? "blocked" : "ready")}
            </div>
            <div class="row-body">${escapeHtml(compactId(identity.public_id || owner.public_id, 32))}</div>
            <div class="subtle">controller ${escapeHtml(compactId(owner.controller_id || owner.controller, 24))} | ${escapeHtml(valueOrDash(profile.role))}</div>
          </div>
        `;
      });
    }

    function walletSummaryRows(rows) {
      return rows.map(([label, value]) => `
        <div class="row">
          <div class="row-head"><div class="row-title">${escapeHtml(label)}</div></div>
          <div class="row-body">${escapeHtml(valueOrDash(value))}</div>
        </div>
      `).join("");
    }

    function chainNetwork(chainId) {
      return chainLabels[String(chainId || "").toLowerCase()] || "";
    }

    function networkChainId(network) {
      const targetNetwork = String(network || "").toLowerCase();
      return Object.entries(chainLabels).find(([, label]) => label === targetNetwork)?.[0] || "";
    }

    function renderWalletNetworkOptions(activeNetwork) {
      const selectedNetwork = String(activeNetwork || "base").toLowerCase();
      const knownNetworks = new Set(walletNetworkOptions.map((option) => option.value));
      const options = knownNetworks.has(selectedNetwork)
        ? walletNetworkOptions
        : [{ value: selectedNetwork, label: selectedNetwork }, ...walletNetworkOptions];
      return options.map((option) => {
        const selected = option.value === selectedNetwork ? " selected" : "";
        return `<option value="${escapeHtml(option.value)}"${selected}>${escapeHtml(option.label)}</option>`;
      }).join("");
    }

    function walletPaymentAccounts(operator = currentWalletOperator) {
      const accounts = safeArray(operator?.payment_accounts);
      const active = operator?.active_payment_account;
      if (!active?.account_id || accounts.some((account) => account.account_id === active.account_id)) {
        return accounts;
      }
      return [active, ...accounts];
    }

    function walletPaymentAccountFor(network, rail, operator = currentWalletOperator) {
      const targetNetwork = String(network || "").toLowerCase();
      const targetRail = String(rail || "x402").toLowerCase();
      return walletPaymentAccounts(operator).find((account) => (
        String(account?.network || "").toLowerCase() === targetNetwork
        && String(account?.rail || "x402").toLowerCase() === targetRail
        && account?.can_sign
      )) || null;
    }

    function stablecoinTokensFor(networkRef) {
      const key = String(networkRef || "").toLowerCase();
      if (stablecoinContracts[key]) return stablecoinContracts[key];
      const chainId = Object.entries(chainLabels).find(([, label]) => label === key)?.[0];
      return chainId ? stablecoinContracts[chainId] || [] : [];
    }

    function balanceOfData(address) {
      return `0x70a08231${String(address || "").replace(/^0x/i, "").padStart(64, "0")}`;
    }

    async function rpcCall(rpcUrl, method, params) {
      const response = await fetch(rpcUrl, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }),
      });
      const payload = await response.json();
      if (!response.ok || payload.error) {
        throw new Error(payload.error?.message || `RPC request failed with ${response.status}`);
      }
      return payload.result;
    }

    function formatTokenAmount(hexValue, decimals) {
      const raw = BigInt(hexValue || "0x0");
      const scale = 10n ** BigInt(decimals);
      const whole = raw / scale;
      const fraction = raw % scale;
      const fractionText = fraction.toString().padStart(decimals, "0").slice(0, 4).replace(/0+$/, "");
      return fractionText ? `${whole}.${fractionText}` : whole.toString();
    }

    function renderTokenBalances(networkRef, balances = {}) {
      const tokens = stablecoinTokensFor(networkRef);
      const list = qs("web3-token-balances");
      if (!tokens.length) {
        list.innerHTML = `<div class="empty">No stablecoin contracts configured for this network.</div>`;
        return;
      }
      list.innerHTML = tokens.map((token) => `
        <div class="token-balance">
          <span>${escapeHtml(token.symbol)}</span>
          <strong>${escapeHtml(balances[token.symbol] || "-")}</strong>
          <span>${escapeHtml(chainNetwork(networkRef) || networkRef)}</span>
        </div>
      `).join("");
    }

    async function refreshStablecoinBalances() {
      const selectedNetwork = qs("web3-wallet-network")?.value.trim() || selectedWalletNetwork || "base";
      const selectedRail = qs("web3-wallet-rail")?.value.trim() || "x402";
      const selectedChainId = networkChainId(selectedNetwork);
      const rpcUrl = stablecoinRpcUrls[selectedChainId];
      const payment = walletPaymentAccountFor(selectedNetwork, selectedRail);
      const address = payment?.address || "";
      if (!address) {
        setStatus("Create an agent wallet first.", true);
        return;
      }
      if (!rpcUrl) {
        setStatus(`Balance refresh RPC is not configured for ${selectedNetwork}.`, true);
        renderTokenBalances(selectedNetwork);
        return;
      }
      const tokens = stablecoinTokensFor(selectedNetwork);
      renderTokenBalances(selectedNetwork);
      const balances = {};
      await Promise.all(tokens.map(async (token) => {
        const result = await rpcCall(
          rpcUrl,
          "eth_call",
          [{
            to: token.address,
            data: balanceOfData(address),
          }, "latest"],
        );
        balances[token.symbol] = formatTokenAmount(result, token.decimals);
      }));
      renderTokenBalances(selectedNetwork, balances);
    }

    async function createAgentWallet() {
      const address = qs("web3-wallet-address").value.trim();
      const chainId = connectedWeb3Wallet?.chainId || "";
      const network = qs("web3-wallet-network").value.trim() || chainNetwork(chainId);
      const rail = qs("web3-wallet-rail").value.trim() || "x402";
      selectedWalletNetwork = network || "base";
      const data = await fetchJson("/v1/wallet/payment-account/create", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          network,
          rail,
          label: "agent-wallet",
        }),
        auth: true,
      });
      const activeAddress = data.active_payment_account?.address || address;
      selectedWalletNetwork = data.active_payment_account?.network || selectedWalletNetwork;
      setStatus(data.already_exists
        ? `Agent wallet already exists ${compactId(activeAddress, 28)}.`
        : `Created agent wallet ${compactId(activeAddress, 28)}.`);
      await refreshConsole();
    }

    function bindWalletControls() {
      qs("create-agent-wallet")?.addEventListener("click", () => {
        createAgentWallet().catch((error) => setStatus(error.message, true));
      });
      qs("refresh-web3-balances")?.addEventListener("click", () => {
        refreshStablecoinBalances().catch((error) => setStatus(error.message, true));
      });
      qs("web3-wallet-network")?.addEventListener("change", (event) => {
        selectedWalletNetwork = event.target.value || "base";
        renderWallet(currentWalletOperator || {});
      });
    }

    function renderWallet(operator) {
      currentWalletOperator = operator;
      const chainId = connectedWeb3Wallet?.chainId || "";
      const activeNetwork = selectedWalletNetwork || chainNetwork(chainId) || "base";
      const fallbackRail = operator.active_payment_account?.rail || "x402";
      const selectedPayment = walletPaymentAccountFor(activeNetwork, fallbackRail, operator);
      const activeAddress = selectedPayment?.address || "";
      const hasSigningAccount = Boolean(selectedPayment?.can_sign);
      qs("wallet-list").innerHTML = `
        <section class="wallet-section">
          <div class="wallet-section-head">
            <div class="wallet-section-title">WATT Internal Ledger</div>
            ${pill("local", "ready")}
          </div>
          ${walletSummaryRows([
            ["WATT", operator.watt_balance],
            ["Reward Policy", operator.reward_policy_version],
            ["Wallet Agent DID", operator.wallet_bound_agent_did],
            ["Controller", operator.controller_id],
          ])}
        </section>
        <section class="wallet-section web3">
          <div class="wallet-section-head">
            <div class="wallet-section-title">Agent Payment Account</div>
            ${pill(selectedPayment ? "bound" : "unbound", selectedPayment ? "ready" : "pending")}
          </div>
          ${walletSummaryRows([
            ["Payment Account", selectedPayment ? selectedPayment.account_id : "none"],
            ["Address", activeAddress || "none"],
            ["Rail", selectedPayment?.rail || fallbackRail],
            ["Network", activeNetwork || "none"],
            ["Custody", selectedPayment?.custody || "none"],
            ["Can Sign", selectedPayment?.can_sign ? "yes" : "no"],
          ])}
          <div class="wallet-fields">
            <label>
              Address
              <input id="web3-wallet-address" value="${escapeHtml(activeAddress)}" readonly>
            </label>
            <label>
              Network
              <select id="web3-wallet-network">
                ${renderWalletNetworkOptions(activeNetwork)}
              </select>
            </label>
            <label>
              Rail
              <input id="web3-wallet-rail" value="${escapeHtml(selectedPayment?.rail || fallbackRail)}">
            </label>
          </div>
          <div class="wallet-actions">
            <button id="create-agent-wallet" type="button" ${hasSigningAccount ? "disabled" : ""}>Create Agent Wallet</button>
            <button id="refresh-web3-balances" class="secondary" type="button">Refresh Balances</button>
          </div>
          <div id="web3-wallet-status" class="subtle">${escapeHtml(activeAddress ? compactId(activeAddress, 28) : "No agent payment account created.")}</div>
          <div id="web3-token-balances" class="wallet-token-grid"></div>
        </section>
        <section class="wallet-section web2">
          <div class="wallet-section-head">
            <div class="wallet-section-title">Web2 Payments</div>
            ${pill("reserved", "pending")}
          </div>
          ${walletSummaryRows([
            ["Payment Account", "not implemented"],
            ["Payment Kind", "web2 reserved"],
          ])}
        </section>
      `;
      renderTokenBalances(activeNetwork);
      bindWalletControls();
    }

    function renderOrganizations(payload) {
      renderList("organizations-list", safeArray(payload.organizations), "No guilds recorded.", (row) => `
        <div class="row">
          <div class="row-head">
            <div class="row-title">${escapeHtml(row.name || row.id)}</div>
            ${pill(row.status || "org", row.status)}
          </div>
          <div class="row-body">${escapeHtml(valueOrDash(row.member_count))} members | ${escapeHtml(valueOrDash(row.treasury_watt))} WATT | ${escapeHtml(valueOrDash(row.mission_count))} missions</div>
        </div>
      `);
    }

    function renderRpcLogs(payload) {
      renderList("rpc-list", safeArray(payload.rpc_logs), "No recent events recorded.", (row) => `
        <div class="row">
          <div class="row-head">
            <div class="row-title">${escapeHtml(row.message || "event")}</div>
            ${pill(row.level || "info", row.level)}
          </div>
          <div class="row-body">${escapeHtml(formatTime(row.timestamp))}</div>
        </div>
      `);
    }

    function diagnosticIsError(row) {
      const text = `${row.level || ""} ${row.status || ""}`.toLowerCase();
      return text.includes("error") || text.includes("fail") || text.includes("warn");
    }

    function diagnosticIsMcpTool(row) {
      return row.source === "wattetheria"
        && (row.component === "wattetheria.mcp" || row.category === "tool_call");
    }

    function diagnosticIsAgentCallback(row) {
      const phase = String(row.phase || "");
      return row.source === "wattetheria"
        && row.category === "agent_event"
        && (phase.startsWith("callback.") || phase.startsWith("decision."));
    }

    function diagnosticIsEventBus(row) {
      return row.source === "wattetheria"
        && (row.component === "wattetheria.event_bus" || row.category === "agent_action_commit");
    }

    function diagnosticDetails(row) {
      return row && row.details && typeof row.details === "object" && !Array.isArray(row.details)
        ? row.details
        : {};
    }

    function diagnosticNodeId(row) {
      if (!row) return "";
      if (row.object_kind === "node" && row.object_id) return row.object_id;
      if (row.category === "transport" && row.object_id) return row.object_id;
      return row.source_node_id || "";
    }

    function diagnosticTitle(row) {
      const phase = String(row.phase || "");
      if (row.source === "wattswarm" && phase === "connection.established") return "network connection established";
      if (row.source === "wattswarm" && phase === "connection.closed") return "network connection closed";
      if (row.source === "wattswarm" && phase === "handshake.rejected") return "network handshake rejected";
      return row.message || row.phase || "diagnostic";
    }

    function diagnosticContextSummary(row) {
      const details = diagnosticDetails(row);
      const items = [];
      const nodeId = diagnosticNodeId(row);
      if (nodeId) items.push(`node ${compactId(nodeId, 28)}`);
      if (details.remote_addr) items.push(`remote ${details.remote_addr}`);
      if (details.remaining_established != null) items.push(`remaining ${details.remaining_established}`);
      if (details.endpoint_url) items.push(`callback ${details.endpoint_url}`);
      if (details.event_type) items.push(`event type ${details.event_type}`);
      if (details.feed_key) items.push(`feed ${details.feed_key}`);
      if (details.events_applied != null) items.push(`events ${details.events_applied}`);
      if (row.scope_hint) items.push(`scope ${row.scope_hint}`);
      return items.join(" | ");
    }

    function filteredDiagnosticEntries(entries) {
      const explicitSource = qs("diagnostic-source").value.trim();
      return safeArray(entries).filter((row) => {
        if (explicitSource && row.source !== explicitSource) return false;
        if (activeLogMode === "wattetheria" && row.source !== "wattetheria") return false;
        if (activeLogMode === "mcp" && !diagnosticIsMcpTool(row)) return false;
        if (activeLogMode === "callbacks" && !diagnosticIsAgentCallback(row)) return false;
        if (activeLogMode === "eventbus" && !diagnosticIsEventBus(row)) return false;
        if (activeLogMode === "wattswarm" && row.source !== "wattswarm") return false;
        if (activeLogMode === "errors" && !diagnosticIsError(row)) return false;
        return true;
      });
    }

    function renderDiagnostics(payload, entries) {
      const local = payload?.local || {};
      const swarm = payload?.swarm || {};
      const snapshot = swarm && swarm.snapshot ? swarm.snapshot : {};
      const localRows = safeArray(local.entries);
      qs("local-log-count").textContent = valueOrDash(localRows.length);
      qs("mcp-log-count").textContent = valueOrDash(localRows.filter((row) => diagnosticIsMcpTool({ ...row, source: "wattetheria" })).length);
      qs("callback-log-count").textContent = valueOrDash(localRows.filter((row) => diagnosticIsAgentCallback({ ...row, source: "wattetheria" })).length);
      qs("event-bus-log-count").textContent = valueOrDash(localRows.filter((row) => diagnosticIsEventBus({ ...row, source: "wattetheria" })).length);
      qs("local-log-errors").textContent = valueOrDash(localRows.filter(diagnosticIsError).length);
      qs("local-log-last").textContent = localRows.length ? compactId(localRows[0].phase || localRows[0].message || "-", 24) : "-";
      qs("swarm-diag-service").textContent = swarm && swarm.network_service_started ? "running" : "stopped";
      qs("swarm-diag-connected").textContent = valueOrDash(snapshot.connected_node_count || snapshot.known_iroh_contacts || 0);
      qs("swarm-diag-scopes").textContent = valueOrDash(safeArray(snapshot.subscribed_scopes).length);
      const visibleEntries = filteredDiagnosticEntries(entries);
      renderList("diagnostic-list", visibleEntries, "No logs recorded for the current filters.", (row) => {
        const details = diagnosticDetails(row);
        const contextSummary = diagnosticContextSummary(row);
        const meta = [
          row.source_label,
          row.component,
          row.category,
          row.phase,
          row.object_kind && row.object_id ? `${row.object_kind} ${compactId(row.object_id, 24)}` : "",
          row.event_id ? `event ${compactId(row.event_id, 18)}` : "",
          row.source_node_id && row.source_node_id !== row.object_id ? `from ${compactId(row.source_node_id, 18)}` : "",
          details.author_node_id ? `author ${compactId(details.author_node_id, 18)}` : "",
        ].filter(Boolean);
        const timestamp = row.timestamp_ms || row.timestamp || row.generated_at;
        return `
          <div class="row">
            <div class="row-head">
              <div class="row-title">${escapeHtml(diagnosticTitle(row))}</div>
              ${pill(row.source_label || row.source || "log", row.source)}
              ${pill(row.level || "info", row.status || row.level)}
            </div>
            <div class="row-body">${escapeHtml(formatTime(timestamp))}${contextSummary ? ` | ${escapeHtml(contextSummary)}` : ""}</div>
            <div class="row-meta">${meta.map((item) => `<span>${escapeHtml(item)}</span>`).join("")}</div>
            <details class="row-details">
              <summary>JSON</summary>
              <pre class="code">${escapeHtml(JSON.stringify(row, null, 2))}</pre>
            </details>
          </div>
        `;
      });
    }

    async function loadBrainConfig() {
      if (!tokenEl.value.trim()) {
        document.getElementById("brain-config-status").textContent = "Control token required.";
        document.getElementById("brain-config-status").className = "status-text";
        return;
      }
      try {
        const data = await fetchJson("/v1/brain/config", { auth: true });
        const cfg = data.config || {};
        const kind = (cfg && cfg.kind) || "openai-compatible";
        let runtimeLabel = "not configured";
        if (kind === "openai-compatible") {
          document.getElementById("brain-openai-base-url").value = cfg.base_url || "";
          document.getElementById("brain-openai-model").value = cfg.model || "";
          const apiKeyInput = document.getElementById("brain-api-key");
          apiKeyInput.value = "";
          apiKeyInput.placeholder = data.has_api_key
            ? "Configured - enter a new key to replace"
            : "Enter API key";
          runtimeLabel = data.label || kind;
        }
        document.getElementById("brain-provider-label").textContent = runtimeLabel;
        document.getElementById("side-runtime").textContent = runtimeLabel;
        document.getElementById("brain-config-status").textContent = "";
        document.getElementById("brain-config-status").className = "status-text";
      } catch (err) {
        document.getElementById("brain-config-status").textContent = "Load failed: " + err.message;
        document.getElementById("brain-config-status").className = "status-text error";
      }
    }

    async function saveBrainConfig() {
      const kind = "openai-compatible";
      const body = { kind };
      body.base_url = document.getElementById("brain-openai-base-url").value.trim();
      body.model = document.getElementById("brain-openai-model").value.trim();
      const apiKey = document.getElementById("brain-api-key").value.trim();
      if (apiKey) body.api_key = apiKey;
      try {
        const data = await fetchJson("/v1/brain/config", {
          method: "PUT",
          headers: { "content-type": "application/json" },
          body: JSON.stringify(body),
          auth: true,
        });
        document.getElementById("brain-config-status").textContent = data.ok
          ? "Saved to deploy env. Restart required."
          : "Error";
        document.getElementById("brain-config-status").className = data.ok ? "status-text ok" : "status-text error";
        document.getElementById("brain-provider-label").textContent = data.label || "";
        document.getElementById("side-runtime").textContent = data.label || kind;
        if (data.ok) {
          const apiKeyInput = document.getElementById("brain-api-key");
          apiKeyInput.value = "";
          apiKeyInput.placeholder = data.has_api_key
            ? "Configured - enter a new key to replace"
            : "Enter API key";
        }
      } catch (err) {
        document.getElementById("brain-config-status").textContent = "Save failed: " + err.message;
        document.getElementById("brain-config-status").className = "status-text error";
      }
    }

    document.getElementById("load-identities").addEventListener("click", loadIdentities);
    document.getElementById("refresh").addEventListener("click", refreshConsole);
    document.getElementById("save-settings").addEventListener("click", saveSettings);
    document.getElementById("refresh-diagnostics").addEventListener("click", () => {
      refreshDiagnostics().catch((error) => setStatus(error.message, true));
    });
    document.getElementById("export-diagnostics").addEventListener("click", exportDiagnostics);
    qs("missions-search")?.addEventListener("input", (event) => {
      missionSearchQuery = event.target.value;
      missionPage = 1;
      if (lastConsolePayload) renderMissions(lastConsolePayload);
    });
    qs("missions-prev")?.addEventListener("click", () => {
      missionPage = Math.max(1, missionPage - 1);
      if (lastConsolePayload) renderMissions(lastConsolePayload);
    });
    qs("missions-next")?.addEventListener("click", () => {
      missionPage += 1;
      if (lastConsolePayload) renderMissions(lastConsolePayload);
    });
    document.querySelectorAll("[data-log-mode]").forEach((button) => {
      button.addEventListener("click", () => {
        activeLogMode = button.dataset.logMode || "all";
        document.querySelectorAll("[data-log-mode]").forEach((item) => {
          item.classList.toggle("active", item === button);
        });
        renderDiagnostics(lastDiagnosticPayload || { local: {}, swarm: {} }, lastDiagnosticEntries);
      });
    });
    document.querySelectorAll("[data-view]").forEach((link) => {
      link.addEventListener("click", (event) => {
        event.preventDefault();
        showPage(link.dataset.view);
      });
    });
    window.addEventListener("hashchange", () => showPage(pageFromHash(), false));
    tokenEl.addEventListener("change", () => {
      tokenEl.value = normalizeToken(tokenEl.value);
    });
    tokenEl.addEventListener("blur", () => {
      tokenEl.value = normalizeToken(tokenEl.value);
    });

    syncSwarmConsoleLink();
    showPage(pageFromHash(), false);
    loadSettings();
    if (tokenEl.value.trim()) {
      loadIdentities().then(() => {
        if (publicIdEl.value) { refreshConsole(); loadBrainConfig(); }
        else loadBrainConfig();
      });
    }
