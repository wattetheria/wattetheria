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
        const signedSnapshotRequest = fetchJson(`/v1/client/export?${query.toString()}`);
        const localSocialRequest = loadLocalSocialPayload(publicId, limit);
        const diagnosticsRequest = refreshDiagnostics().then(
          () => null,
          (error) => error,
        );
        const [signed, localSocial] = await Promise.all([
          signedSnapshotRequest,
          localSocialRequest,
        ]);
        const payload = signed.payload || signed;
        Object.assign(payload, localSocial);
        renderSnapshot(payload);
        restartMessageRefreshForCurrentView({ immediate: false });
        const diagnosticsError = await diagnosticsRequest;
        if (diagnosticsError) throw diagnosticsError;
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
      const [friendRequestsResult, dmMessagesResult, clientFriendsResult] = await Promise.allSettled([
        fetchJson(`/v1/client/friend-requests?${query.toString()}`, { auth: true }),
        fetchJson(`/v1/client/friends/messages?${query.toString()}`, { auth: true }),
        fetchJson(`/v1/client/friends?${query.toString()}`, { auth: true }),
      ]);
      const friendRequests = friendRequestsResult.status === "fulfilled" ? friendRequestsResult.value : {};
      const dmMessages = dmMessagesResult.status === "fulfilled" ? dmMessagesResult.value : [];
      const clientFriends = clientFriendsResult.status === "fulfilled" ? clientFriendsResult.value : [];
      return {
        local_client_friends: [],
        friend_relationships: safeArray(clientFriends),
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
