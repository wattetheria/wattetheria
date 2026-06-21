    function hiveKey(row) {
      return valueOrDash(row.topic_id || row.hive_id || `${valueOrDash(row.feed_key)}@${valueOrDash(row.scope_hint)}`);
    }

    function hiveTitle(row) {
      return row.display_name || row.title || row.name || row.feed_key || row.topic_id || "Hive";
    }

    function hiveLabel(row) {
      return String(hiveTitle(row)).replace(/^#/, "");
    }

    function hiveMessageCount(row) {
      return valueOrZero(row.recent_message_count || row.message_count || row.messages_count || row.activity_count);
    }

    function hiveMessageTimestamp(row) {
      const value = row && row.created_at;
      if (typeof value === "number" && Number.isFinite(value)) return value;
      if (typeof value === "string") {
        const numeric = Number(value);
        if (Number.isFinite(numeric)) return numeric;
        const parsed = Date.parse(value);
        if (Number.isFinite(parsed)) return parsed;
      }
      return 0;
    }

    function sortHiveMessagesChronologically(rows) {
      return safeArray(rows).slice().sort((left, right) => {
        const byTime = hiveMessageTimestamp(left) - hiveMessageTimestamp(right);
        if (byTime !== 0) return byTime;
        return String(left.message_id || "").localeCompare(String(right.message_id || ""));
      });
    }

    function scrollHiveMessagesToLatest() {
      const messageList = qs("hive-messages-list");
      if (!messageList) return;
      const scroll = () => {
        messageList.scrollTop = messageList.scrollHeight;
      };
      requestAnimationFrame(() => {
        scroll();
        requestAnimationFrame(scroll);
      });
      setTimeout(scroll, 120);
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
          scrollHiveMessagesToLatest();
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
      const hasCached = hiveMessageCache.has(key);
      let rows = hasCached ? safeArray(hiveMessageCache.get(key)) : [];
      if (!hasCached && !loading && !error) {
        rows = fallbackHiveMessages(payload, activeHive);
      }
      rows = sortHiveMessagesChronologically(rows);
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
      scrollHiveMessagesToLatest();
    }

    function fallbackHiveMessages(payload, hive) {
      const rows = safeArray(payload.public_topic_messages);
      const scoped = rows.some((row) => row.topic_id || row.hive_id || row.feed_key || row.scope_hint);
      if (!scoped) return [];
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
