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
