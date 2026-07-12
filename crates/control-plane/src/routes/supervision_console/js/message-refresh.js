    const messageRefreshBaseDelayMs = 10000;
    const messageRefreshMaxDelayMs = 60000;
    let messageRefreshTimer = null;
    let messageRefreshInFlight = false;
    let messageRefreshFailures = 0;

    function captureMessageScroll(element) {
      if (!element) return null;
      const distanceFromBottom = Math.max(0, element.scrollHeight - element.clientHeight - element.scrollTop);
      return {
        followLatest: distanceFromBottom <= 80,
        distanceFromBottom,
      };
    }

    function restoreMessageScroll(element, scrollState) {
      if (!element) return;
      if (!scrollState || scrollState.followLatest) {
        const scroll = () => { element.scrollTop = element.scrollHeight; };
        requestAnimationFrame(() => {
          scroll();
          requestAnimationFrame(scroll);
        });
        setTimeout(scroll, 120);
        return;
      }
      const restore = () => {
        element.scrollTop = Math.max(
          0,
          element.scrollHeight - element.clientHeight - scrollState.distanceFromBottom,
        );
      };
      requestAnimationFrame(() => {
        restore();
        requestAnimationFrame(restore);
      });
    }

    function messageCollectionsEqual(current, next) {
      return JSON.stringify(safeArray(current)) === JSON.stringify(safeArray(next));
    }

    function messageRefreshView() {
      const page = pageFromHash();
      return page === "swarm" || page === "social" ? page : "";
    }

    function messageRefreshCanRun() {
      return document.visibilityState === "visible"
        && navigator.onLine !== false
        && Boolean(publicIdEl.value)
        && Boolean(lastConsolePayload)
        && Boolean(messageRefreshView());
    }

    function nextMessageRefreshDelay() {
      if (!messageRefreshFailures) return messageRefreshBaseDelayMs;
      return Math.min(
        messageRefreshMaxDelayMs,
        messageRefreshBaseDelayMs * (2 ** messageRefreshFailures),
      );
    }

    function stopMessageRefresh() {
      if (messageRefreshTimer !== null) {
        clearTimeout(messageRefreshTimer);
        messageRefreshTimer = null;
      }
    }

    function scheduleMessageRefresh(delayMs = nextMessageRefreshDelay()) {
      stopMessageRefresh();
      if (!messageRefreshCanRun()) return;
      messageRefreshTimer = window.setTimeout(runMessageRefresh, delayMs);
    }

    async function refreshActiveHiveMessages() {
      const hives = safeArray(lastConsolePayload?.public_topics);
      const activeHive = hives.find((row) => hiveKey(row) === activeHiveKey);
      if (!activeHive) return true;
      const outcome = await loadHiveMessages(activeHive, { silent: true });
      return outcome.ok;
    }

    async function refreshDmMessagesOnly() {
      const publicId = publicIdEl.value;
      const payload = lastConsolePayload;
      if (!publicId || !payload) return true;
      const query = new URLSearchParams({
        public_id: publicId,
        limit: String(Math.max(1, Math.min(Number(limitEl.value) || 50, 200))),
      });
      const messages = safeArray(await fetchJson(
        `/v1/client/friends/messages?${query.toString()}`,
        { auth: true },
      ));
      if (publicIdEl.value !== publicId || lastConsolePayload !== payload) return true;
      if (messageCollectionsEqual(payload.dm_messages, messages)) return true;

      const scrollState = captureMessageScroll(qs("dm-list")?.querySelector(".dm-bubble-list"));
      payload.dm_messages = messages;
      renderDmMessages(payload, { scrollState });
      return true;
    }

    async function runMessageRefresh() {
      messageRefreshTimer = null;
      if (!messageRefreshCanRun() || messageRefreshInFlight) {
        scheduleMessageRefresh();
        return;
      }

      messageRefreshInFlight = true;
      try {
        const refreshed = messageRefreshView() === "swarm"
          ? await refreshActiveHiveMessages()
          : await refreshDmMessagesOnly();
        messageRefreshFailures = refreshed ? 0 : messageRefreshFailures + 1;
      } catch (_) {
        messageRefreshFailures += 1;
      } finally {
        messageRefreshInFlight = false;
        scheduleMessageRefresh();
      }
    }

    function restartMessageRefreshForCurrentView(options = {}) {
      stopMessageRefresh();
      messageRefreshFailures = 0;
      if (messageRefreshCanRun()) {
        scheduleMessageRefresh(options.immediate === false ? messageRefreshBaseDelayMs : 0);
      }
    }

    function initMessageRefresh() {
      document.addEventListener("visibilitychange", restartMessageRefreshForCurrentView);
      window.addEventListener("online", restartMessageRefreshForCurrentView);
      window.addEventListener("offline", stopMessageRefresh);
      window.addEventListener("beforeunload", stopMessageRefresh);
      restartMessageRefreshForCurrentView();
    }
