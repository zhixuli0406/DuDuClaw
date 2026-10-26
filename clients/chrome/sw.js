// Service worker — context-menu clipper + side-panel opener. All network I/O
// lives in the side panel / options pages; the worker only shuttles the
// user's page selection into session storage for the panel to pick up.
const NEW_GATEWAY = 'http://127.0.0.1:18889';
const LEGACY_GATEWAY = 'http://127.0.0.1:18789';

async function isDuduClawGateway(base) {
  try {
    const response = await fetch(base + '/healthz', {
      signal: AbortSignal.timeout(500),
    });
    const body = await response.json();
    return body?.service === 'duduclaw-gateway';
  } catch {
    return false;
  }
}

chrome.runtime.onInstalled.addListener(async (details) => {
  const { gatewayUrl, accessToken } = await chrome.storage.local.get([
    'gatewayUrl',
    'accessToken',
  ]);
  if (!gatewayUrl) {
    if (details.reason === 'install') {
      await chrome.storage.local.set({ gatewayUrl: NEW_GATEWAY });
    } else {
      const [legacyLive, currentLive] = await Promise.all([
        isDuduClawGateway(LEGACY_GATEWAY),
        isDuduClawGateway(NEW_GATEWAY),
      ]);
      // Existing credentials stay bound to the verified legacy endpoint. If
      // it is offline, leave migration pending rather than sending the token
      // to a different service or instance.
      const selected = legacyLive
        ? LEGACY_GATEWAY
        : !accessToken && currentLive
          ? NEW_GATEWAY
          : null;
      await chrome.storage.local.set({
        gatewayUrl: selected,
        gatewayMigrationPending: !selected,
      });
    }
  }
  chrome.contextMenus.create({
    id: 'duduclaw-clip',
    title: '把選取內容傳給 DuDuClaw',
    contexts: ['selection'],
  });
  chrome.sidePanel
    .setPanelBehavior({ openPanelOnActionClick: true })
    .catch(() => {});
});

chrome.contextMenus.onClicked.addListener(async (info, tab) => {
  if (info.menuItemId !== 'duduclaw-clip' || !tab?.id) return;
  const clip = {
    text: info.selectionText || '',
    url: info.pageUrl || tab.url || '',
    title: tab.title || '',
    at: Date.now(),
  };
  await chrome.storage.session.set({ pendingClip: clip });
  try {
    await chrome.sidePanel.open({ tabId: tab.id });
  } catch {
    // Side panel may fail on restricted pages; the clip stays queued and the
    // panel picks it up next time it opens.
  }
});
