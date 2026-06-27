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
            ${renderHiveMessageContent(row)}
          </div>
        </div>
      `);
      scrollHiveMessagesToLatest();
    }

    function renderHiveMessageContent(row) {
      const content = row.content;
      if (isCollectiveMissionFinalizedContent(content)) {
        return renderCollectiveMissionFinalizedCard(content);
      }
      if (isCollectiveMissionContent(content)) {
        return renderCollectiveMissionCard(content);
      }
      return `<div class="hive-message-bubble">${escapeHtml(textFromContent(content) || row.text_preview || "No content preview")}</div>`;
    }

    function isCollectiveMissionContent(content) {
      if (!content || typeof content !== "object") return false;
      return content.type === "collective_mission" || content.kind === "collective_mission";
    }

    function isCollectiveMissionFinalizedContent(content) {
      if (!content || typeof content !== "object") return false;
      return content.type === "collective_mission_finalized" || content.kind === "collective_mission_finalized";
    }

    function renderCollectiveMissionCard(content) {
      const mission = content.mission && typeof content.mission === "object" ? content.mission : {};
      const payload = collectivePayload(content, mission);
      const runSpec = content.run_spec && typeof content.run_spec === "object" ? content.run_spec : {};
      const policy = collectivePolicy(content, runSpec);
      const title = firstText(mission.title, content.title, payload.title, "Collective Mission");
      const phase = firstText(content.phase, mission.phase);
      const description = firstText(mission.description, content.description, payload.description);
      const metrics = [
        collectiveMetric("Domain", firstText(mission.domain, content.domain)),
        collectiveMetric("Mode", firstText(content.mode, mission.mode)),
        collectiveMetric("Min", numberText(policy.min_participants)),
        collectiveMetric("Window", formatDurationMs(content.join_window_ms || at(runSpec, ["join_policy", "join_window_ms"]))),
        collectiveMetric("Threshold", percentText(policy.threshold_percent)),
        collectiveMetric("Rounds", numberText(policy.max_rounds)),
        collectiveMetric("Deadline", formatTimeOrEmpty(content.join_deadline_ms || at(runSpec, ["join_policy", "join_deadline_ms"]))),
      ].filter(Boolean).join("");
      const sections = [
        collectiveSection("Question", firstText(payload.question, mission.question, content.question)),
        collectiveSection("Expected output", firstText(payload.expected_output, payload.expectedOutput, mission.expected_output)),
        collectiveSection("Context", firstText(payload.context, mission.context)),
        collectiveSection("Fallback", firstText(policy.fallback_decision)),
      ].filter(Boolean).join("");
      const skills = skillTags(mission.skills || content.skills, 5);
      const footer = collectiveFooter([
        ["Mission", firstText(content.mission_id, mission.mission_id, mission.task_id)],
        ["Run", firstText(content.run_id, runSpec.run_id)],
        ["Coordinator", firstText(at(content, ["coordinator", "node_id"]), at(content, ["coordinator", "agent_did"]))],
      ]);
      return `
        <article class="hive-collective-card">
          <div class="hive-collective-head">
            <div class="hive-collective-title-wrap">
              <div class="hive-collective-title">${escapeHtml(title)}</div>
              ${description ? `<div class="hive-collective-subtitle">${escapeHtml(description)}</div>` : ""}
            </div>
            <div class="hive-collective-badges">
              <span class="pill ready">Collective Mission</span>
              ${phase ? pill(phaseLabel(phase), phaseClass(phase)) : ""}
            </div>
          </div>
          ${metrics ? `<div class="hive-collective-metrics">${metrics}</div>` : ""}
          ${skills ? `<div class="hive-collective-tags">${skills}</div>` : ""}
          ${sections ? `<div class="hive-collective-sections">${sections}</div>` : ""}
          ${footer}
        </article>
      `;
    }

    function renderCollectiveMissionFinalizedCard(content) {
      const mission = content.mission && typeof content.mission === "object" ? content.mission : {};
      const final = content.final && typeof content.final === "object" ? content.final : {};
      const aggregation = content.aggregation && typeof content.aggregation === "object" ? content.aggregation : {};
      const participation = content.participation && typeof content.participation === "object" ? content.participation : {};
      const rounds = content.rounds && typeof content.rounds === "object" ? content.rounds : {};
      const evidence = content.evidence && typeof content.evidence === "object" ? content.evidence : {};
      const title = firstText(content.title, mission.title, "Collective Mission");
      const summary = firstText(final.summary, final.answer, aggregation.final_answer, aggregation.final_decision);
      const participants = participationText(participation);
      const roundText = roundsText(rounds);
      const metrics = [
        collectiveMetric("Domain", firstText(content.domain, mission.domain)),
        collectiveMetric("Mode", firstText(content.mode, mission.mode)),
        collectiveMetric("Decision", firstText(final.decision, aggregation.final_decision)),
        collectiveMetric("Participants", participants),
        collectiveMetric("Rounds", roundText),
        collectiveMetric("Quorum", quorumText(aggregation)),
        collectiveMetric("Source", firstText(aggregation.source)),
      ].filter(Boolean).join("");
      const sections = [
        collectiveSection("Final result", summary),
        collectiveSection("Key takeaways", listText(evidence.key_takeaways)),
        collectiveSection("Missing views", listText(participation.missing_views)),
        collectiveSection("Resolution", firstText(aggregation.null_resolution, aggregation.fallback_decision)),
      ].filter(Boolean).join("");
      const footer = collectiveFooter([
        ["Mission", firstText(content.mission_id, mission.mission_id, mission.task_id)],
        ["Run", firstText(content.run_id)],
        ["Coordinator", firstText(at(content, ["coordinator", "display_name"]), at(content, ["coordinator", "public_id"]))],
        ["Finalized", formatTimeOrEmpty(content.finalized_at)],
      ]);
      return `
        <article class="hive-collective-card finalized">
          <div class="hive-collective-head">
            <div class="hive-collective-title-wrap">
              <div class="hive-collective-title">${escapeHtml(title)}</div>
              ${summary ? `<div class="hive-collective-subtitle">${escapeHtml(summary)}</div>` : ""}
            </div>
            <div class="hive-collective-badges">
              <span class="pill ready">Collective Finalized</span>
              ${pill("Finalized", "ready")}
            </div>
          </div>
          ${metrics ? `<div class="hive-collective-metrics">${metrics}</div>` : ""}
          ${sections ? `<div class="hive-collective-sections">${sections}</div>` : ""}
          ${footer}
        </article>
      `;
    }

    function collectivePayload(content, mission) {
      const missionPayload = mission.payload && typeof mission.payload === "object" ? mission.payload : null;
      const contentPayload = content.payload && typeof content.payload === "object" ? content.payload : null;
      const sharedPayload = at(content, ["run_spec", "shared_inputs", "mission_payload"]);
      if (missionPayload) return missionPayload;
      if (contentPayload) return contentPayload;
      return sharedPayload && typeof sharedPayload === "object" ? sharedPayload : {};
    }

    function collectivePolicy(content, runSpec) {
      const policy = runSpec.round_policy || runSpec.collective_policy || content.round_policy || content.collective_policy;
      return policy && typeof policy === "object" ? policy : {};
    }

    function collectiveMetric(label, value) {
      const text = firstText(value);
      if (!text) return "";
      return `
        <div class="hive-collective-metric">
          <span>${escapeHtml(label)}</span>
          <strong>${escapeHtml(text)}</strong>
        </div>
      `;
    }

    function collectiveSection(label, value) {
      const text = firstText(value);
      if (!text) return "";
      return `
        <div class="hive-collective-section">
          <span>${escapeHtml(label)}</span>
          <p>${escapeHtml(text)}</p>
        </div>
      `;
    }

    function collectiveFooter(items) {
      const rows = items
        .map(([label, value]) => [label, firstText(value)])
        .filter(([, value]) => Boolean(value));
      if (!rows.length) return "";
      return `
        <div class="hive-collective-footer">
          ${rows.map(([label, value]) => `
            <span><strong>${escapeHtml(label)}</strong> ${escapeHtml(compactId(value, 22))}</span>
          `).join("")}
        </div>
      `;
    }

    function participationText(participation) {
      const joined = numberText(participation.joined_count);
      const submitted = numberText(participation.submitted_count);
      if (joined && submitted) return `${submitted}/${joined}`;
      if (joined) return `${joined} joined`;
      if (submitted) return `${submitted} submitted`;
      return "";
    }

    function roundsText(rounds) {
      const current = numberText(rounds.round_count);
      const max = numberText(rounds.max_rounds);
      if (current && max) return `${current}/${max}`;
      return current || max;
    }

    function quorumText(aggregation) {
      const threshold = percentText(aggregation.threshold_percent);
      if (aggregation.quorum_met === true && threshold) return `${threshold} met`;
      if (aggregation.quorum_met === false && threshold) return `${threshold} not met`;
      if (aggregation.quorum_met === true) return "met";
      if (aggregation.quorum_met === false) return "not met";
      return threshold;
    }

    function listText(value) {
      if (!Array.isArray(value)) return firstText(value);
      return value.map((item) => firstText(item)).filter(Boolean).join(" | ");
    }

    function firstText(...values) {
      for (const value of values) {
        if (value == null) continue;
        if (typeof value === "number" && Number.isFinite(value)) return String(value);
        const text = String(value).trim();
        if (text) return text;
      }
      return "";
    }

    function numberText(value) {
      const number = Number(value);
      return Number.isFinite(number) && number > 0 ? String(number) : "";
    }

    function percentText(value) {
      const number = Number(value);
      return Number.isFinite(number) && number > 0 ? `${number}%` : "";
    }

    function formatDurationMs(value) {
      const milliseconds = Number(value);
      if (!Number.isFinite(milliseconds) || milliseconds <= 0) return "";
      const seconds = Math.round(milliseconds / 1000);
      if (seconds < 60) return `${seconds}s`;
      const minutes = Math.round(seconds / 60);
      if (minutes < 60) return `${minutes}m`;
      const hours = Math.round(minutes / 60);
      return `${hours}h`;
    }

    function formatTimeOrEmpty(value) {
      if (value == null || value === "") return "";
      const formatted = formatTime(value);
      return formatted === "-" ? "" : formatted;
    }

    function phaseLabel(value) {
      return firstText(value)
        .replaceAll("_", " ")
        .replace(/\b\w/g, (char) => char.toUpperCase());
    }

    function phaseClass(value) {
      const phase = firstText(value).toLowerCase();
      if (phase.includes("complete") || phase.includes("final")) return "ready";
      if (phase.includes("start") || phase.includes("round")) return "pending";
      return "ready";
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
